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
    pub lattice: Option<Arc<crate::converter::Lattice>>,
}

pub(crate) struct CandidateResult {
    pub reading: String,
    pub response: CandidateResponse,
}

/// Sink invoked by the worker thread when a candidate generation completes.
/// Implementations must be cheap and non-blocking; heavy work should be
/// dispatched to another thread from inside the sink.
pub(crate) trait CandidateSink: Send + Sync + 'static {
    fn deliver(&self, result: CandidateResult);
}

// ---------------------------------------------------------------------------
// AsyncWorker
// ---------------------------------------------------------------------------

pub(crate) struct AsyncWorker {
    // Resources captured at construction; consumed when the worker thread
    // is lazily spawned on the first `submit_candidates` call.
    dict: Arc<dyn Dictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,
    sink: Arc<dyn CandidateSink>,

    candidate_gen: Arc<AtomicU64>,
    inner: Mutex<WorkerInner>,
}

struct WorkerInner {
    tx: Option<mpsc::Sender<CandidateWork>>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl AsyncWorker {
    pub fn new(
        dict: Arc<dyn Dictionary>,
        conn: Option<Arc<ConnectionMatrix>>,
        history: Option<Arc<RwLock<UserHistory>>>,
        sink: Arc<dyn CandidateSink>,
    ) -> Self {
        Self {
            dict,
            conn,
            history,
            sink,
            candidate_gen: Arc::new(AtomicU64::new(0)),
            inner: Mutex::new(WorkerInner {
                tx: None,
                thread_handle: None,
            }),
        }
    }

    pub fn submit_candidates(
        &self,
        reading: String,
        dispatch: CandidateDispatch,
        lattice: Option<Arc<crate::converter::Lattice>>,
    ) {
        let gen = self.candidate_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let mut inner = self.inner.lock().unwrap();
        if inner.tx.is_none() {
            // Lazy spawn: IMKit instantiates probe controllers that never
            // request candidates, so we avoid spawning threads until the
            // first real candidate request arrives.
            let (tx, rx) = mpsc::channel::<CandidateWork>();
            let dict = Arc::clone(&self.dict);
            let conn = self.conn.clone();
            let history = self.history.clone();
            let sink = Arc::clone(&self.sink);
            let gen_ref = Arc::clone(&self.candidate_gen);
            let handle = thread::Builder::new()
                .name("lexime-candidates".into())
                .spawn(move || {
                    candidate_worker(rx, sink, gen_ref, dict, conn, history);
                })
                .expect("failed to spawn candidate worker");
            inner.tx = Some(tx);
            inner.thread_handle = Some(handle);
        }
        if let Some(ref tx) = inner.tx {
            let _ = tx.send(CandidateWork {
                reading,
                dispatch,
                generation: gen,
                lattice,
            });
        }
    }

    pub fn invalidate_candidates(&self) {
        self.candidate_gen.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for AsyncWorker {
    fn drop(&mut self) {
        // Drop the sender first so the worker thread's recv() returns Err and exits.
        let mut inner = self.inner.lock().unwrap();
        inner.tx.take();
        if let Some(handle) = inner.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker threads
// ---------------------------------------------------------------------------

fn candidate_worker(
    rx: mpsc::Receiver<CandidateWork>,
    sink: Arc<dyn CandidateSink>,
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
        let response = if let Some(ref lattice) = latest.lattice {
            match latest.dispatch {
                CandidateDispatch::Predictive => {
                    crate::candidates::generate_prediction_candidates_from_lattice(
                        lattice,
                        &*dict,
                        conn_ref,
                        hist_ref,
                        max_results,
                    )
                }
                CandidateDispatch::Standard => crate::candidates::generate_candidates_from_lattice(
                    lattice,
                    &*dict,
                    conn_ref,
                    hist_ref,
                    max_results,
                ),
            }
        } else {
            match latest.dispatch {
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
            }
        };

        // Check staleness after generation
        if latest.generation != gen.load(Ordering::SeqCst) {
            continue;
        }

        // Use lattice.input as the canonical reading when a lattice was provided,
        // so the stale-check in receive_candidates matches the actual conversion.
        let reading = match latest.lattice {
            Some(lattice) => lattice.input.clone(),
            None => latest.reading,
        };
        let result = CandidateResult { reading, response };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.deliver(result))).is_err()
        {
            tracing::error!("candidate worker: CandidateSink::deliver panicked; dropping result");
        }
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
