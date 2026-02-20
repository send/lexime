// Re-export lex-core: api/ の use crate::xxx パスを変更不要にする
pub use lex_core::candidates;
pub use lex_core::converter;
pub use lex_core::dict;
pub use lex_core::romaji;
pub use lex_core::settings;
pub use lex_core::unicode;
pub use lex_core::user_dict;
pub use lex_core::user_history;

// Re-export lex-session: api/ の use crate::session::* を変更不要にする
pub use lex_session as session;

pub mod api;
pub(crate) mod async_worker;
pub mod trace_init;

uniffi::setup_scaffolding!();
