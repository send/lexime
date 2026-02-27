use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;

use crate::candidates::CandidateResponse;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::session::CandidateDispatch;
use crate::settings::settings;
use crate::user_history::UserHistory;

// ---------------------------------------------------------------------------
// Work / Result types
// ---------------------------------------------------------------------------

pub(crate) struct CandidateWork {
    pub reading: String,
    pub dispatch: CandidateDispatch,
    pub generation: u64,
}

pub(crate) struct CandidateResult {
    pub reading: String,
    pub response: CandidateResponse,
}

// ---------------------------------------------------------------------------
// AsyncWorker
// ---------------------------------------------------------------------------

pub(crate) struct AsyncWorker {
    candidate_tx: Option<mpsc::Sender<CandidateWork>>,
    candidate_rx: Mutex<mpsc::Receiver<CandidateResult>>,
    candidate_gen: Arc<AtomicU64>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl AsyncWorker {
    pub fn new(
        dict: Arc<dyn Dictionary>,
        conn: Option<Arc<ConnectionMatrix>>,
        history: Option<Arc<RwLock<UserHistory>>>,
    ) -> Self {
        let candidate_gen = Arc::new(AtomicU64::new(0));

        // Candidate worker
        let (work_tx, work_rx) = mpsc::channel::<CandidateWork>();
        let (result_tx, result_rx) = mpsc::channel::<CandidateResult>();
        let handle = {
            let dict = Arc::clone(&dict);
            let conn = conn.clone();
            let history = history.clone();
            let gen = Arc::clone(&candidate_gen);
            thread::Builder::new()
                .name("lexime-candidates".into())
                .spawn(move || {
                    candidate_worker(work_rx, result_tx, gen, dict, conn, history);
                })
                .expect("failed to spawn candidate worker")
        };

        Self {
            candidate_tx: Some(work_tx),
            candidate_rx: Mutex::new(result_rx),
            candidate_gen,
            thread_handle: Some(handle),
        }
    }

    pub fn submit_candidates(&self, reading: String, dispatch: CandidateDispatch) {
        let gen = self.candidate_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(ref tx) = self.candidate_tx {
            let _ = tx.send(CandidateWork {
                reading,
                dispatch,
                generation: gen,
            });
        }
    }

    pub fn invalidate_candidates(&self) {
        self.candidate_gen.fetch_add(1, Ordering::SeqCst);
    }

    pub fn try_recv_candidate(&self) -> Option<CandidateResult> {
        let rx = self.candidate_rx.lock().ok()?;
        rx.try_recv().ok()
    }
}

impl Drop for AsyncWorker {
    fn drop(&mut self) {
        // Drop the sender first so the worker thread's recv() returns Err and exits.
        self.candidate_tx.take();
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker threads
// ---------------------------------------------------------------------------

fn candidate_worker(
    rx: mpsc::Receiver<CandidateWork>,
    tx: mpsc::Sender<CandidateResult>,
    gen: Arc<AtomicU64>,
    dict: Arc<dyn Dictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,
) {
    while let Ok(work) = rx.recv() {
        // Drain: if multiple work items queued, skip to latest
        let mut latest = work;
        while let Ok(newer) = rx.try_recv() {
            latest = newer;
        }

        // Check staleness before doing work
        if latest.generation != gen.load(Ordering::SeqCst) {
            continue;
        }

        let h_guard = history.as_ref().and_then(|h| h.read().ok());
        let hist_ref = h_guard.as_deref();
        let conn_ref = conn.as_deref();

        let max_results = settings().candidates.max_results;
        let response = match latest.dispatch {
            CandidateDispatch::Predictive => crate::candidates::generate_prediction_candidates(
                &*dict,
                conn_ref,
                hist_ref,
                &latest.reading,
                max_results,
            ),
            CandidateDispatch::Standard => crate::candidates::generate_candidates(
                &*dict,
                conn_ref,
                hist_ref,
                &latest.reading,
                max_results,
            ),
        };

        // Check staleness after generation
        if latest.generation != gen.load(Ordering::SeqCst) {
            continue;
        }

        let _ = tx.send(CandidateResult {
            reading: latest.reading,
            response,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generation_counter_invalidation() {
        let gen = Arc::new(AtomicU64::new(0));
        assert_eq!(gen.load(Ordering::SeqCst), 0);
        gen.fetch_add(1, Ordering::SeqCst);
        assert_eq!(gen.load(Ordering::SeqCst), 1);
        // Work with generation 0 is now stale
        assert_ne!(0u64, gen.load(Ordering::SeqCst));
    }
}
