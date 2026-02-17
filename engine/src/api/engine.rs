use std::sync::Arc;

use super::{LexConnection, LexDictionary, LexError, LexNeuralScorer, LexSession, LexUserHistory};

#[derive(uniffi::Object)]
pub struct LexEngine {
    dict: Arc<LexDictionary>,
    conn: Option<Arc<LexConnection>>,
    history: Option<Arc<LexUserHistory>>,
    neural: Option<Arc<LexNeuralScorer>>,
}

#[uniffi::export]
impl LexEngine {
    #[uniffi::constructor]
    fn new(
        dict: Arc<LexDictionary>,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
        neural: Option<Arc<LexNeuralScorer>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            dict,
            conn,
            history,
            neural,
        })
    }

    fn create_session(&self) -> Arc<LexSession> {
        LexSession::new(
            Arc::clone(&self.dict),
            self.conn.as_ref().map(Arc::clone),
            self.history.as_ref().map(Arc::clone),
            self.neural.as_ref().map(Arc::clone),
        )
    }

    fn save_history(&self, path: String) -> Result<(), LexError> {
        match &self.history {
            Some(h) => h.save(path),
            None => Ok(()),
        }
    }

    fn has_neural(&self) -> bool {
        self.neural.is_some()
    }
}
