use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use crate::candidates::CandidateResponse;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;
use crate::user_history::UserHistory;

// ---------------------------------------------------------------------------
// Work / Result types
// ---------------------------------------------------------------------------

pub(crate) struct CandidateWork {
    pub reading: String,
    pub dispatch: u8,
    #[allow(dead_code)] // used only with neural feature
    pub context: String,
    pub generation: u64,
}

pub(crate) struct CandidateResult {
    pub reading: String,
    pub response: CandidateResponse,
}

pub(crate) struct GhostWork {
    #[allow(dead_code)] // used only with neural feature
    pub context: String,
    pub generation: u64,
}

pub(crate) struct GhostResult {
    pub generation: u64,
    pub text: String,
}

// ---------------------------------------------------------------------------
// AsyncWorker
// ---------------------------------------------------------------------------

pub(crate) struct AsyncWorker {
    candidate_tx: mpsc::Sender<CandidateWork>,
    candidate_rx: Mutex<mpsc::Receiver<CandidateResult>>,
    candidate_gen: Arc<AtomicU64>,

    ghost_tx: mpsc::Sender<GhostWork>,
    ghost_rx: Mutex<mpsc::Receiver<GhostResult>>,
    ghost_gen: Arc<AtomicU64>,
}

impl AsyncWorker {
    pub fn new(
        dict: Arc<TrieDictionary>,
        conn: Option<Arc<ConnectionMatrix>>,
        history: Option<Arc<RwLock<UserHistory>>>,
        #[cfg(feature = "neural")] neural: Option<Arc<Mutex<crate::neural::NeuralScorer>>>,
    ) -> Self {
        let candidate_gen = Arc::new(AtomicU64::new(0));
        let ghost_gen = Arc::new(AtomicU64::new(0));

        // Candidate worker
        let (work_tx, work_rx) = mpsc::channel::<CandidateWork>();
        let (result_tx, result_rx) = mpsc::channel::<CandidateResult>();
        {
            let dict = Arc::clone(&dict);
            let conn = conn.clone();
            let history = history.clone();
            let gen = Arc::clone(&candidate_gen);
            #[cfg(feature = "neural")]
            let neural = neural.clone();
            thread::Builder::new()
                .name("lexime-candidates".into())
                .spawn(move || {
                    candidate_worker(
                        work_rx,
                        result_tx,
                        gen,
                        dict,
                        conn,
                        history,
                        #[cfg(feature = "neural")]
                        neural,
                    );
                })
                .expect("failed to spawn candidate worker");
        }

        // Ghost worker
        let (ghost_work_tx, ghost_work_rx) = mpsc::channel::<GhostWork>();
        let (ghost_result_tx, ghost_result_rx) = mpsc::channel::<GhostResult>();
        {
            let gen = Arc::clone(&ghost_gen);
            #[cfg(feature = "neural")]
            let neural = neural;
            thread::Builder::new()
                .name("lexime-ghost".into())
                .spawn(move || {
                    ghost_worker(
                        ghost_work_rx,
                        ghost_result_tx,
                        gen,
                        #[cfg(feature = "neural")]
                        neural,
                    );
                })
                .expect("failed to spawn ghost worker");
        }

        Self {
            candidate_tx: work_tx,
            candidate_rx: Mutex::new(result_rx),
            candidate_gen,
            ghost_tx: ghost_work_tx,
            ghost_rx: Mutex::new(ghost_result_rx),
            ghost_gen,
        }
    }

    pub fn submit_candidates(&self, reading: String, dispatch: u8, context: String) {
        let gen = self.candidate_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.candidate_tx.send(CandidateWork {
            reading,
            dispatch,
            context,
            generation: gen,
        });
    }

    pub fn submit_ghost(&self, context: String, generation: u64) {
        self.ghost_gen.store(generation, Ordering::SeqCst);
        let _ = self.ghost_tx.send(GhostWork {
            context,
            generation,
        });
    }

    pub fn invalidate_candidates(&self) {
        self.candidate_gen.fetch_add(1, Ordering::SeqCst);
    }

    pub fn invalidate_ghost(&self) {
        self.ghost_gen.fetch_add(1, Ordering::SeqCst);
    }

    pub fn try_recv_candidate(&self) -> Option<CandidateResult> {
        let rx = self.candidate_rx.lock().ok()?;
        rx.try_recv().ok()
    }

    pub fn try_recv_ghost(&self) -> Option<GhostResult> {
        let rx = self.ghost_rx.lock().ok()?;
        rx.try_recv().ok()
    }
}

// ---------------------------------------------------------------------------
// Worker threads
// ---------------------------------------------------------------------------

fn candidate_worker(
    rx: mpsc::Receiver<CandidateWork>,
    tx: mpsc::Sender<CandidateResult>,
    gen: Arc<AtomicU64>,
    dict: Arc<TrieDictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,
    #[cfg(feature = "neural")] neural: Option<Arc<Mutex<crate::neural::NeuralScorer>>>,
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

        let response = match latest.dispatch {
            #[cfg(feature = "neural")]
            2 => {
                if let Some(ref neural) = neural {
                    if let Ok(mut scorer) = neural.lock() {
                        crate::candidates::generate_neural_candidates(
                            &mut scorer,
                            &dict,
                            conn_ref,
                            hist_ref,
                            &latest.context,
                            &latest.reading,
                            20,
                        )
                    } else {
                        crate::candidates::generate_candidates(
                            &dict,
                            conn_ref,
                            hist_ref,
                            &latest.reading,
                            20,
                        )
                    }
                } else {
                    crate::candidates::generate_candidates(
                        &dict,
                        conn_ref,
                        hist_ref,
                        &latest.reading,
                        20,
                    )
                }
            }
            #[cfg(not(feature = "neural"))]
            2 => crate::candidates::generate_candidates(
                &dict,
                conn_ref,
                hist_ref,
                &latest.reading,
                20,
            ),
            1 => crate::candidates::generate_prediction_candidates(
                &dict,
                conn_ref,
                hist_ref,
                &latest.reading,
                20,
            ),
            _ => crate::candidates::generate_candidates(
                &dict,
                conn_ref,
                hist_ref,
                &latest.reading,
                20,
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

fn ghost_worker(
    rx: mpsc::Receiver<GhostWork>,
    #[allow(unused_variables)] tx: mpsc::Sender<GhostResult>,
    gen: Arc<AtomicU64>,
    #[cfg(feature = "neural")] neural: Option<Arc<Mutex<crate::neural::NeuralScorer>>>,
) {
    while let Ok(work) = rx.recv() {
        // Drain to latest
        let mut latest = work;
        while let Ok(newer) = rx.try_recv() {
            latest = newer;
        }

        // Debounce: wait 150ms, then check if still current
        thread::sleep(Duration::from_millis(150));
        if latest.generation != gen.load(Ordering::SeqCst) {
            continue;
        }

        #[cfg(feature = "neural")]
        {
            let Some(ref neural) = neural else { continue };
            let Ok(mut scorer) = neural.lock() else {
                continue;
            };
            let config = crate::neural::GenerateConfig {
                max_tokens: 30,
                ..crate::neural::GenerateConfig::default()
            };
            let Ok(text) = scorer.generate_text(&latest.context, &config) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            // Final staleness check after generation
            if latest.generation != gen.load(Ordering::SeqCst) {
                continue;
            }
            let _ = tx.send(GhostResult {
                generation: latest.generation,
                text,
            });
        }

        #[cfg(not(feature = "neural"))]
        {
            let _ = &latest;
            // No neural scorer available; nothing to generate
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
