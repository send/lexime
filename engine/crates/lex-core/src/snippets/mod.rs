mod config;
mod store;
mod variables;

pub use config::{parse_snippets_toml, SnippetConfigError};
pub use store::SnippetStore;
pub use variables::{SnippetVariable, VariableResolver};
