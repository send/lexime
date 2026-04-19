use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};
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
    candidate_tx: Option<mpsc::Sender<CandidateWork>>,
    candidate_gen: Arc<AtomicU64>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl AsyncWorker {
    pub fn new<S: CandidateSink>(
        dict: Arc<dyn Dictionary>,
        conn: Option<Arc<ConnectionMatrix>>,
        history: Option<Arc<RwLock<UserHistory>>>,
        sink: S,
    ) -> Self {
        let candidate_gen = Arc::new(AtomicU64::new(0));

        // Candidate worker
        let (work_tx, work_rx) = mpsc::channel::<CandidateWork>();
        let handle = {
            let dict = Arc::clone(&dict);
            let conn = conn.clone();
            let history = history.clone();
            let gen = Arc::clone(&candidate_gen);
            thread::Builder::new()
                .name("lexime-candidates".into())
                .spawn(move || {
                    candidate_worker(work_rx, sink, gen, dict, conn, history);
                })
                .expect("failed to spawn candidate worker")
        };

        Self {
            candidate_tx: Some(work_tx),
            candidate_gen,
            thread_handle: Some(handle),
        }
    }

    pub fn submit_candidates(
        &self,
        reading: String,
        dispatch: CandidateDispatch,
        lattice: Option<Arc<crate::converter::Lattice>>,
    ) {
        let gen = self.candidate_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(ref tx) = self.candidate_tx {
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
        self.candidate_tx.take();
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker threads
// ---------------------------------------------------------------------------

fn candidate_worker<S: CandidateSink>(
    rx: mpsc::Receiver<CandidateWork>,
    sink: S,
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

        // Release the history read guard BEFORE delivering the result.
        // `sink.deliver` ultimately calls `LexSession::record_history`, which
        // acquires a write lock on the same `UserHistory` RwLock. Holding a
        // read guard here while asking for a write on the same thread would
        // self-deadlock (std's RwLock is not reentrant).
        drop(h_guard);

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
    use crate::user_history::UserHistory;

    #[test]
    fn test_generation_counter_invalidation() {
        let gen = Arc::new(AtomicU64::new(0));
        assert_eq!(gen.load(Ordering::SeqCst), 0);
        gen.fetch_add(1, Ordering::SeqCst);
        assert_eq!(gen.load(Ordering::SeqCst), 1);
        // Work with generation 0 is now stale
        assert_ne!(0u64, gen.load(Ordering::SeqCst));
    }

    /// Regression guard: the candidate worker must release its history read
    /// guard before invoking the sink. Sinks call back into
    /// `LexSession::record_history`, which acquires a write lock on the same
    /// RwLock — holding the read guard there self-deadlocks on the worker
    /// thread.
    #[test]
    fn sink_can_write_history_during_deliver() {
        struct WritingSink {
            history: Arc<RwLock<UserHistory>>,
            delivered: Arc<std::sync::Mutex<bool>>,
        }
        impl CandidateSink for WritingSink {
            fn deliver(&self, _result: CandidateResult) {
                // Emulate `record_history` acquiring the write lock on the
                // same RwLock the worker read from. This would deadlock if
                // the worker still held its read guard.
                let mut h = self
                    .history
                    .write()
                    .expect("write lock must be acquirable inside deliver");
                h.record_at(&[("きょう".to_string(), "今日".to_string())], 0);
                *self.delivered.lock().unwrap() = true;
            }
        }

        let history = Arc::new(RwLock::new(UserHistory::new()));
        let delivered = Arc::new(std::sync::Mutex::new(false));
        let sink = WritingSink {
            history: Arc::clone(&history),
            delivered: Arc::clone(&delivered),
        };

        let (tx, rx) = mpsc::channel::<CandidateWork>();
        let gen = Arc::new(AtomicU64::new(0));
        let dict: Arc<dyn crate::dict::Dictionary> =
            Arc::new(crate::dict::TrieDictionary::from_entries(std::iter::empty()));

        let worker = thread::spawn({
            let gen = Arc::clone(&gen);
            let history = Arc::clone(&history);
            move || candidate_worker(rx, sink, gen, dict, None, Some(history))
        });

        let work_gen = gen.fetch_add(1, Ordering::SeqCst) + 1;
        tx.send(CandidateWork {
            reading: "きょう".to_string(),
            dispatch: crate::session::CandidateDispatch::Standard,
            generation: work_gen,
            lattice: None,
        })
        .unwrap();

        // Wait bounded time for the deliver to complete. If the guard is
        // still held, the worker self-deadlocks and `delivered` stays false.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if *delivered.lock().unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            *delivered.lock().unwrap(),
            "sink.deliver did not complete in time — worker is likely holding \
             the history read guard across the callback and self-deadlocking"
        );

        drop(tx);
        worker.join().unwrap();
    }
}
