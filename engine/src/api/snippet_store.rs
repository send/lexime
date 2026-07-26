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

#[uniffi::export]
impl LexSnippetStore {
    /// Keys the picker will not offer because their body expands to nothing.
    ///
    /// The store keeps the file and drops what it cannot use, so without asking
    /// for this the frontend would list and count an entry the user can never
    /// select. Returned across the boundary rather than logged: engine `tracing`
    /// output does not reach the shipped build.
    pub fn unusable_keys(&self) -> Vec<String> {
        self.inner.unusable_keys()
    }
}
