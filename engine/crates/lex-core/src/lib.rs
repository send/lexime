#[cfg(not(target_endian = "little"))]
compile_error!("lex-core requires a little-endian platform");

pub mod candidates;
pub mod converter;
pub mod dict;
#[cfg(feature = "neural")]
pub mod neural;
pub(crate) mod numeric;
pub mod romaji;
pub mod settings;
pub mod unicode;
pub mod user_dict;
pub mod user_history;
