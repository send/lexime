use std::sync::Arc;

use super::{
    LexConnection, LexDictionary, LexError, LexNeuralScorer, LexSession, LexUserDictionary,
    LexUserHistory, LexUserWord,
};

#[derive(uniffi::Object)]
pub struct LexEngine {
    dict: Arc<LexDictionary>,
    conn: Option<Arc<LexConnection>>,
    history: Option<Arc<LexUserHistory>>,
    neural: Option<Arc<LexNeuralScorer>>,
    user_dict: Option<Arc<LexUserDictionary>>,
}

#[uniffi::export]
impl LexEngine {
    #[uniffi::constructor]
    fn new(
        dict: Arc<LexDictionary>,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
        neural: Option<Arc<LexNeuralScorer>>,
        user_dict: Option<Arc<LexUserDictionary>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            dict,
            conn,
            history,
            neural,
            user_dict,
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

    fn register_word(&self, reading: String, surface: String) -> bool {
        match &self.user_dict {
            Some(ud) => ud.inner.register(&reading, &surface),
            None => false,
        }
    }

    fn unregister_word(&self, reading: String, surface: String) -> bool {
        match &self.user_dict {
            Some(ud) => ud.inner.unregister(&reading, &surface),
            None => false,
        }
    }

    fn list_user_words(&self) -> Vec<LexUserWord> {
        match &self.user_dict {
            Some(ud) => ud
                .inner
                .list()
                .into_iter()
                .map(|(reading, surface)| LexUserWord { reading, surface })
                .collect(),
            None => Vec::new(),
        }
    }

    fn save_user_dict(&self, path: String) -> Result<(), LexError> {
        match &self.user_dict {
            Some(ud) => ud
                .inner
                .save(std::path::Path::new(&path))
                .map_err(|e| LexError::Io { msg: e.to_string() }),
            None => Ok(()),
        }
    }
}
