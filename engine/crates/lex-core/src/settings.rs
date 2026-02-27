//! Global settings loaded from TOML, following the same OnceLock pattern as romaji config.
//!
//! - `init_custom(toml_content)` sets a custom TOML before first `settings()` call
//! - `settings()` returns `&'static Settings` (lazy-init singleton)
//! - Default values are embedded via `include_str!("default_settings.toml")`

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::snippets::SnippetVariable;

pub const DEFAULT_SETTINGS_TOML: &str = include_str!("default_settings.toml");

static CUSTOM_TOML: OnceLock<String> = OnceLock::new();

/// Set custom TOML before first `settings()` call.
pub fn init_custom(toml_content: String) -> Result<(), SettingsError> {
    parse_settings_toml(&toml_content)?;
    CUSTOM_TOML
        .set(toml_content)
        .map_err(|_| SettingsError::AlreadyInitialized)
}

/// Get or initialize the global settings singleton.
pub fn settings() -> &'static Settings {
    static INSTANCE: OnceLock<Settings> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let toml_str = CUSTOM_TOML
            .get()
            .map(|s| s.as_str())
            .unwrap_or(DEFAULT_SETTINGS_TOML);
        parse_settings_toml(toml_str).expect("settings TOML must be valid")
    })
}

/// Returns the embedded default settings TOML content.
pub fn default_toml() -> &'static str {
    DEFAULT_SETTINGS_TOML
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("TOML parse error: {0}")]
    Parse(String),
    #[error("invalid value for {field}: {reason}")]
    InvalidValue { field: String, reason: String },
    #[error("settings already initialized")]
    AlreadyInitialized,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub cost: CostSettings,
    pub reranker: RerankerSettings,
    pub history: HistorySettings,
    pub candidates: CandidateSettings,
    #[serde(default)]
    pub snippets: SnippetSettings,
    #[serde(default)]
    keymap: HashMap<String, Vec<String>>,
    /// Parsed keymap: key_code → (normal, shifted).
    #[serde(skip)]
    keymap_parsed: Vec<(u16, String, String)>,
}

impl Settings {
    /// Look up a remapped key by key_code and shift state.
    pub fn keymap_get(&self, key_code: u16, has_shift: bool) -> Option<&str> {
        self.keymap_parsed
            .iter()
            .find_map(|(code, normal, shifted)| {
                if *code == key_code {
                    Some(if has_shift {
                        shifted.as_str()
                    } else {
                        normal.as_str()
                    })
                } else {
                    None
                }
            })
    }

    /// Parse the snippet trigger string into a structured key descriptor.
    pub fn snippet_trigger(&self) -> Option<TriggerKey> {
        parse_trigger_string(&self.snippets.trigger)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CostSettings {
    pub segment_penalty: i64,
    pub mixed_script_bonus: i64,
    pub katakana_penalty: i64,
    pub pure_kanji_bonus: i64,
    pub latin_penalty: i64,
    pub unknown_word_cost: i16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RerankerSettings {
    pub length_variance_weight: i64,
    pub structure_cost_filter: i64,
    #[serde(default = "default_non_independent_kanji_penalty")]
    pub non_independent_kanji_penalty: i64,
    #[serde(default = "default_te_form_kanji_penalty")]
    pub te_form_kanji_penalty: i64,
    #[serde(default = "default_pronoun_cost_bonus")]
    pub pronoun_cost_bonus: i64,
    #[serde(default = "default_single_char_kanji_penalty")]
    pub single_char_kanji_penalty: i64,
}

fn default_non_independent_kanji_penalty() -> i64 {
    3000
}

fn default_te_form_kanji_penalty() -> i64 {
    3500
}

fn default_pronoun_cost_bonus() -> i64 {
    3500
}

fn default_single_char_kanji_penalty() -> i64 {
    4000
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistorySettings {
    pub boost_per_use: i64,
    pub max_boost: i64,
    pub half_life_hours: f64,
    pub max_unigrams: usize,
    pub max_bigrams: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CandidateSettings {
    pub nbest: usize,
    pub max_results: usize,
}

fn default_snippet_trigger() -> String {
    "ctrl+shift+@".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SnippetSettings {
    #[serde(default = "default_snippet_trigger")]
    pub trigger: String,
    #[serde(default)]
    pub variables: HashMap<String, SnippetVariable>,
}

/// Parsed trigger key descriptor for character-based matching.
#[derive(Debug, Clone)]
pub struct TriggerKey {
    pub char: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
}

/// Parse a trigger string like "ctrl+;" into a TriggerKey.
/// Format: optional modifiers separated by '+', final segment is the character.
fn parse_trigger_string(s: &str) -> Option<TriggerKey> {
    if s.is_empty() {
        return None;
    }

    let parts: Vec<&str> = s.split('+').collect();
    if parts.is_empty() {
        return None;
    }

    let char_part = parts.last()?.to_string();
    if char_part.is_empty() {
        return None;
    }

    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut cmd = false;

    for &part in &parts[..parts.len() - 1] {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" | "opt" => alt = true,
            "cmd" | "command" | "super" => cmd = true,
            _ => return None, // unknown modifier
        }
    }

    Some(TriggerKey {
        char: char_part,
        ctrl,
        shift,
        alt,
        cmd,
    })
}

pub fn parse_settings_toml(toml_str: &str) -> Result<Settings, SettingsError> {
    let mut s: Settings =
        toml::from_str(toml_str).map_err(|e| SettingsError::Parse(e.to_string()))?;
    validate(&s)?;
    s.keymap_parsed = parse_keymap(&s.keymap)?;
    Ok(s)
}

fn parse_keymap(
    raw: &HashMap<String, Vec<String>>,
) -> Result<Vec<(u16, String, String)>, SettingsError> {
    let mut result = Vec::new();
    for (key_str, values) in raw {
        let key_code: u16 = key_str.parse().map_err(|_| SettingsError::InvalidValue {
            field: format!("keymap.{}", key_str),
            reason: "key_code must be a u16 integer".to_string(),
        })?;
        if values.len() != 2 {
            return Err(SettingsError::InvalidValue {
                field: format!("keymap.{}", key_str),
                reason: "value must be [\"normal\", \"shifted\"]".to_string(),
            });
        }
        result.push((key_code, values[0].clone(), values[1].clone()));
    }
    Ok(result)
}

fn validate(s: &Settings) -> Result<(), SettingsError> {
    macro_rules! check_non_negative {
        ($section:ident . $field:ident) => {
            if s.$section.$field < 0 {
                return Err(SettingsError::InvalidValue {
                    field: concat!(stringify!($section), ".", stringify!($field)).to_string(),
                    reason: "must be non-negative".to_string(),
                });
            }
        };
    }
    macro_rules! check_positive_usize {
        ($section:ident . $field:ident) => {
            if s.$section.$field == 0 {
                return Err(SettingsError::InvalidValue {
                    field: concat!(stringify!($section), ".", stringify!($field)).to_string(),
                    reason: "must be positive".to_string(),
                });
            }
        };
    }

    check_non_negative!(cost.segment_penalty);
    check_non_negative!(cost.mixed_script_bonus);
    check_non_negative!(cost.katakana_penalty);
    check_non_negative!(cost.pure_kanji_bonus);
    check_non_negative!(cost.latin_penalty);
    check_non_negative!(cost.unknown_word_cost);

    check_non_negative!(reranker.length_variance_weight);
    check_non_negative!(reranker.structure_cost_filter);
    check_non_negative!(reranker.non_independent_kanji_penalty);
    check_non_negative!(reranker.te_form_kanji_penalty);
    check_non_negative!(reranker.pronoun_cost_bonus);
    check_non_negative!(reranker.single_char_kanji_penalty);

    check_non_negative!(history.boost_per_use);
    check_non_negative!(history.max_boost);
    check_positive_usize!(history.max_unigrams);
    check_positive_usize!(history.max_bigrams);
    if s.history.half_life_hours <= 0.0 {
        return Err(SettingsError::InvalidValue {
            field: "history.half_life_hours".to_string(),
            reason: "must be positive".to_string(),
        });
    }

    check_positive_usize!(candidates.nbest);
    check_positive_usize!(candidates.max_results);

    // i16 range check for unknown_word_cost is enforced by the type itself

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_toml() {
        let s = parse_settings_toml(DEFAULT_SETTINGS_TOML).unwrap();
        assert_eq!(s.cost.segment_penalty, 5000);
        assert_eq!(s.cost.mixed_script_bonus, 3000);
        assert_eq!(s.cost.katakana_penalty, 5000);
        assert_eq!(s.cost.pure_kanji_bonus, 1000);
        assert_eq!(s.cost.latin_penalty, 20000);
        assert_eq!(s.cost.unknown_word_cost, 10000);
        assert_eq!(s.reranker.length_variance_weight, 2000);
        assert_eq!(s.reranker.structure_cost_filter, 4000);
        assert_eq!(s.reranker.non_independent_kanji_penalty, 3000);
        assert_eq!(s.reranker.te_form_kanji_penalty, 3500);
        assert_eq!(s.reranker.pronoun_cost_bonus, 3500);
        assert_eq!(s.reranker.single_char_kanji_penalty, 4000);
        assert_eq!(s.history.boost_per_use, 3000);
        assert_eq!(s.history.max_boost, 15000);
        assert!((s.history.half_life_hours - 168.0).abs() < f64::EPSILON);
        assert_eq!(s.history.max_unigrams, 10000);
        assert_eq!(s.history.max_bigrams, 10000);
        assert_eq!(s.candidates.nbest, 20);
        assert_eq!(s.candidates.max_results, 20);
        // Snippet defaults
        assert_eq!(s.snippets.trigger, "ctrl+shift+@");
        let trigger = s.snippet_trigger().unwrap();
        assert_eq!(trigger.char, "@");
        assert!(trigger.ctrl);
        assert!(trigger.shift);
        assert!(!trigger.alt);
        assert!(!trigger.cmd);
        // Keymap defaults
        assert_eq!(s.keymap_get(10, false), Some("]"));
        assert_eq!(s.keymap_get(10, true), Some("}"));
        assert_eq!(s.keymap_get(93, false), Some("\\"));
        assert_eq!(s.keymap_get(93, true), Some("|"));
        assert_eq!(s.keymap_get(999, false), None);
    }

    #[test]
    fn parse_valid_custom_toml() {
        let toml = r#"
[cost]
segment_penalty = 1000
mixed_script_bonus = 500
katakana_penalty = 2000
pure_kanji_bonus = 200
latin_penalty = 10000
unknown_word_cost = 5000

[reranker]
length_variance_weight = 1000
structure_cost_filter = 2000
non_independent_kanji_penalty = 3000

[history]
boost_per_use = 1500
max_boost = 8000
half_life_hours = 72.0
max_unigrams = 5000
max_bigrams = 5000

[candidates]
nbest = 10
max_results = 30
"#;
        let s = parse_settings_toml(toml).unwrap();
        assert_eq!(s.cost.segment_penalty, 1000);
        assert_eq!(s.candidates.nbest, 10);
    }

    #[test]
    fn error_negative_penalty() {
        let toml = r#"
[cost]
segment_penalty = -1
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000

[reranker]
length_variance_weight = 2000
structure_cost_filter = 4000
non_independent_kanji_penalty = 3000

[history]
boost_per_use = 3000
max_boost = 15000
half_life_hours = 168.0
max_unigrams = 10000
max_bigrams = 10000

[candidates]
nbest = 5
max_results = 20
"#;
        let err = parse_settings_toml(toml).unwrap_err();
        assert!(matches!(err, SettingsError::InvalidValue { .. }));
        assert!(err.to_string().contains("cost.segment_penalty"));
    }

    #[test]
    fn error_zero_half_life() {
        let toml = r#"
[cost]
segment_penalty = 5000
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000

[reranker]
length_variance_weight = 2000
structure_cost_filter = 4000
non_independent_kanji_penalty = 3000

[history]
boost_per_use = 3000
max_boost = 15000
half_life_hours = 0.0
max_unigrams = 10000
max_bigrams = 10000

[candidates]
nbest = 5
max_results = 20
"#;
        let err = parse_settings_toml(toml).unwrap_err();
        assert!(err.to_string().contains("half_life_hours"));
    }

    #[test]
    fn error_zero_nbest() {
        let toml = r#"
[cost]
segment_penalty = 5000
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000

[reranker]
length_variance_weight = 2000
structure_cost_filter = 4000
non_independent_kanji_penalty = 3000

[history]
boost_per_use = 3000
max_boost = 15000
half_life_hours = 168.0
max_unigrams = 10000
max_bigrams = 10000

[candidates]
nbest = 0
max_results = 20
"#;
        let err = parse_settings_toml(toml).unwrap_err();
        assert!(err.to_string().contains("candidates.nbest"));
    }

    #[test]
    fn keymap_omitted_is_empty() {
        let toml = r#"
[cost]
segment_penalty = 5000
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000

[reranker]
length_variance_weight = 2000
structure_cost_filter = 4000
non_independent_kanji_penalty = 3000

[history]
boost_per_use = 3000
max_boost = 15000
half_life_hours = 168.0
max_unigrams = 10000
max_bigrams = 10000

[candidates]
nbest = 5
max_results = 20
"#;
        let s = parse_settings_toml(toml).unwrap();
        assert_eq!(s.keymap_get(10, false), None);
        assert_eq!(s.keymap_get(93, false), None);
    }

    #[test]
    fn error_keymap_invalid_key_code() {
        let toml = r#"
[cost]
segment_penalty = 5000
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000

[reranker]
length_variance_weight = 2000
structure_cost_filter = 4000
non_independent_kanji_penalty = 3000

[history]
boost_per_use = 3000
max_boost = 15000
half_life_hours = 168.0
max_unigrams = 10000
max_bigrams = 10000

[candidates]
nbest = 5
max_results = 20

[keymap]
abc = ["]", "}"]
"#;
        let err = parse_settings_toml(toml).unwrap_err();
        assert!(err.to_string().contains("keymap.abc"));
    }

    #[test]
    fn parse_trigger_basic() {
        let t = parse_trigger_string("ctrl+;").unwrap();
        assert_eq!(t.char, ";");
        assert!(t.ctrl);
        assert!(!t.shift);
        assert!(!t.alt);
        assert!(!t.cmd);
    }

    #[test]
    fn parse_trigger_multiple_modifiers() {
        let t = parse_trigger_string("ctrl+shift+@").unwrap();
        assert_eq!(t.char, "@");
        assert!(t.ctrl);
        assert!(t.shift);
    }

    #[test]
    fn parse_trigger_single_char() {
        let t = parse_trigger_string(";").unwrap();
        assert_eq!(t.char, ";");
        assert!(!t.ctrl);
        assert!(!t.shift);
        assert!(!t.alt);
        assert!(!t.cmd);
    }

    #[test]
    fn parse_trigger_alias_modifiers() {
        let t = parse_trigger_string("option+command+a").unwrap();
        assert_eq!(t.char, "a");
        assert!(t.alt);
        assert!(t.cmd);
        assert!(!t.ctrl);
    }

    #[test]
    fn parse_trigger_empty_returns_none() {
        assert!(parse_trigger_string("").is_none());
    }

    #[test]
    fn parse_trigger_trailing_plus_returns_none() {
        // "ctrl+" splits into ["ctrl", ""] — empty char part
        assert!(parse_trigger_string("ctrl+").is_none());
    }

    #[test]
    fn parse_trigger_unknown_modifier_returns_none() {
        assert!(parse_trigger_string("meta+;").is_none());
    }

    #[test]
    fn error_invalid_toml() {
        let err = parse_settings_toml("not valid toml {{{").unwrap_err();
        assert!(matches!(err, SettingsError::Parse(_)));
    }

    #[test]
    fn error_missing_section() {
        let toml = r#"
[cost]
segment_penalty = 5000
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000
"#;
        let err = parse_settings_toml(toml).unwrap_err();
        assert!(matches!(err, SettingsError::Parse(_)));
    }
}
