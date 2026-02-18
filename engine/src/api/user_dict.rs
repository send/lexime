use std::path::Path;
use std::sync::Arc;

use crate::user_dict::UserDictionary;

use super::{LexError, LexUserWord};

#[derive(uniffi::Object)]
pub struct LexUserDictionary {
    pub(crate) inner: Arc<UserDictionary>,
}

#[uniffi::export]
impl LexUserDictionary {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let dict = UserDictionary::open(Path::new(&path))
            .map_err(|e| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(dict),
        }))
    }

    fn register(&self, reading: String, surface: String) -> bool {
        self.inner.register(&reading, &surface)
    }

    fn unregister(&self, reading: String, surface: String) -> bool {
        self.inner.unregister(&reading, &surface)
    }

    fn list(&self) -> Vec<LexUserWord> {
        self.inner
            .list()
            .into_iter()
            .map(|(reading, surface)| LexUserWord { reading, surface })
            .collect()
    }

    fn save(&self, path: String) -> Result<(), LexError> {
        self.inner
            .save(Path::new(&path))
            .map_err(|e| LexError::Io { msg: e.to_string() })
    }
}
