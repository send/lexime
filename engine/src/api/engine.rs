use std::sync::Arc;

use super::{
    LexConnection, LexDictionary, LexError, LexSession, LexUserDictionary, LexUserHistory,
    LexUserWord,
};

#[derive(uniffi::Object)]
pub struct LexEngine {
    dict: Arc<LexDictionary>,
    conn: Option<Arc<LexConnection>>,
    history: Option<Arc<LexUserHistory>>,
    user_dict: Option<Arc<LexUserDictionary>>,
}

#[uniffi::export]
impl LexEngine {
    #[uniffi::constructor]
    fn new(
        dict: Arc<LexDictionary>,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
        user_dict: Option<Arc<LexUserDictionary>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            dict,
            conn,
            history,
            user_dict,
        })
    }

    fn create_session(&self) -> Arc<LexSession> {
        LexSession::new(
            Arc::clone(&self.dict),
            self.conn.as_ref().map(Arc::clone),
            self.history.as_ref().map(Arc::clone),
        )
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

    /// Clear all learning history (in-memory, WAL, and checkpoint files).
    fn clear_history(&self) -> Result<(), LexError> {
        match &self.history {
            Some(h) => h.clear_impl(),
            None => Ok(()),
        }
    }
}
