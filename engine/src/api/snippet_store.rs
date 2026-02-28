use std::sync::Arc;

use lex_core::snippets::SnippetStore;

/// FFI wrapper around SnippetStore.
#[derive(uniffi::Object)]
pub struct LexSnippetStore {
    pub(crate) inner: Arc<SnippetStore>,
}

impl LexSnippetStore {
    pub(crate) fn new(inner: Arc<SnippetStore>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}
