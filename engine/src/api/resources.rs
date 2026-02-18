use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::dict::connection::ConnectionMatrix;
use crate::dict::{Dictionary, TrieDictionary};
use crate::user_history::UserHistory;

use super::LexError;

#[derive(uniffi::Object)]
pub struct LexDictionary {
    pub(crate) inner: Arc<dyn Dictionary>,
}

#[uniffi::export]
impl LexDictionary {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let dict = TrieDictionary::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(dict),
        }))
    }

    fn lookup(&self, reading: String) -> Vec<super::LexDictEntry> {
        self.inner
            .lookup(&reading)
            .iter()
            .map(|e| super::LexDictEntry {
                reading: reading.clone(),
                surface: e.surface.clone(),
                cost: e.cost,
            })
            .collect()
    }
}

#[derive(uniffi::Object)]
pub struct LexConnection {
    pub(crate) inner: Arc<ConnectionMatrix>,
}

#[uniffi::export]
impl LexConnection {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let conn = ConnectionMatrix::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(conn),
        }))
    }
}

#[derive(uniffi::Object)]
pub struct LexUserHistory {
    pub(crate) inner: Arc<RwLock<UserHistory>>,
}

#[uniffi::export]
impl LexUserHistory {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let history = UserHistory::open(Path::new(&path))
            .map_err(|e: std::io::Error| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(RwLock::new(history)),
        }))
    }

    pub(super) fn save(&self, path: String) -> Result<(), LexError> {
        let h = self
            .inner
            .read()
            .map_err(|e| LexError::Internal { msg: e.to_string() })?;
        h.save(Path::new(&path))
            .map_err(|e| LexError::Io { msg: e.to_string() })
    }
}
