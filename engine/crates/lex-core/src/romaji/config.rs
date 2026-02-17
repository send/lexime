use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize)]
struct RomajiConfig {
    mappings: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RomajiConfigError {
    #[error("TOML parse error: {0}")]
    Parse(String),
    #[error("[mappings] table is empty")]
    Empty,
    #[error("non-ASCII key: {0}")]
    NonAsciiKey(String),
    #[error("empty value for key: {0}")]
    EmptyValue(String),
    #[error("romaji trie already initialized")]
    AlreadyInitialized,
}

/// Parse TOML text into a sorted `BTreeMap<romaji, kana>`.
pub fn parse_romaji_toml(toml_str: &str) -> Result<BTreeMap<String, String>, RomajiConfigError> {
    let config: RomajiConfig =
        toml::from_str(toml_str).map_err(|e| RomajiConfigError::Parse(e.to_string()))?;

    if config.mappings.is_empty() {
        return Err(RomajiConfigError::Empty);
    }

    for (key, value) in &config.mappings {
        if !key.is_ascii() {
            return Err(RomajiConfigError::NonAsciiKey(key.clone()));
        }
        if value.is_empty() {
            return Err(RomajiConfigError::EmptyValue(key.clone()));
        }
    }

    Ok(config.mappings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_toml() {
        let toml = r#"
[mappings]
a = "あ"
ka = "か"
"#;
        let map = parse_romaji_toml(toml).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["a"], "あ");
        assert_eq!(map["ka"], "か");
    }

    #[test]
    fn parse_default_toml() {
        let map = parse_romaji_toml(super::super::table::DEFAULT_TOML).unwrap();
        assert!(map.len() > 250, "expected 250+ mappings, got {}", map.len());
    }

    #[test]
    fn error_empty_mappings() {
        let toml = "[mappings]\n";
        let err = parse_romaji_toml(toml).unwrap_err();
        assert!(matches!(err, RomajiConfigError::Empty));
    }

    #[test]
    fn error_non_ascii_key() {
        let toml = "
[mappings]
\"あ\" = \"a\"
";
        let err = parse_romaji_toml(toml).unwrap_err();
        assert!(matches!(err, RomajiConfigError::NonAsciiKey(_)));
    }

    #[test]
    fn error_empty_value() {
        let toml = r#"
[mappings]
a = ""
"#;
        let err = parse_romaji_toml(toml).unwrap_err();
        assert!(matches!(err, RomajiConfigError::EmptyValue(_)));
    }

    #[test]
    fn error_invalid_toml() {
        let err = parse_romaji_toml("not valid toml {{{").unwrap_err();
        assert!(matches!(err, RomajiConfigError::Parse(_)));
    }
}
