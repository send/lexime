mod config;
mod store;
mod variables;

pub use config::{parse_snippets_toml, validate_snippet_entries, SnippetConfigError};
pub use store::SnippetStore;
pub use variables::{SnippetVariable, VariableResolver};
