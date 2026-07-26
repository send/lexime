//! Global settings loaded from TOML, following the same OnceLock pattern as romaji config.
//!
//! - `init_custom(toml_content)` sets a custom TOML before first `settings()` call
//! - `settings()` returns `&'static Settings` (lazy-init singleton)
//! - Default values are embedded via `include_str!("default_settings.toml")`

use std::collections::{BTreeMap, HashMap};
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
    /// The keymap the engine actually reads, in canonical form: one entry per
    /// key code, sides that are not remaps already dropped. `parse_keymap` is
    /// the only thing that builds it.
    #[serde(skip)]
    keymap_parsed: BTreeMap<u16, KeyRemap>,
    /// What `parse_keymap` dropped while building `keymap_parsed`, phrased for a
    /// human. Recorded there rather than re-derived here, so the report cannot
    /// disagree with the map about what the file does.
    #[serde(skip)]
    keymap_warnings: Vec<String>,
}

/// One key code's remap, as resolved by `parse_keymap`. `None` on a side means
/// that side is not remapped — never "remap to nothing", which would commit `""`
/// and end the host's marked-text session while inserting nothing (SPEC.md §
/// 不変条件（marked text と session の同期）③). `usable_side` is the only thing
/// that fills these in, and it never produces `Some("")`; the type does not
/// forbid it, the single construction site does.
#[derive(Debug, Clone)]
struct KeyRemap {
    normal: Option<String>,
    shifted: Option<String>,
}

impl Settings {
    /// Look up a remapped key by key_code and shift state. `None` = not
    /// remapped, so the caller sends the key through its normal path.
    ///
    /// A lookup, not a search: `parse_keymap` has already picked the single
    /// entry per key code and turned the sides that are not remaps into `None`,
    /// so an unmapped side has no shadowed entry to fall through to.
    pub fn keymap_get(&self, key_code: u16, has_shift: bool) -> Option<&str> {
        let remap = self.keymap_parsed.get(&key_code)?;
        if has_shift {
            remap.shifted.as_deref()
        } else {
            remap.normal.as_deref()
        }
    }

    /// Parse the snippet trigger string into a structured key descriptor.
    pub fn snippet_trigger(&self) -> Option<TriggerKey> {
        parse_trigger_string(&self.snippets.trigger)
    }

    /// Entries the loader kept but will not act on, phrased for a human, so the
    /// caller that owns the config can tell the user.
    ///
    /// Same shape as `SnippetStore::unusable_keys`, down to the crossing: the
    /// load path keeps the file and drops what it cannot use, and a separate
    /// query makes the drop visible. `settings_keymap_warnings` puts this over
    /// the FFI and the frontend reports it after a successful load — engine
    /// `tracing` does not reach the shipped build, so logging here would be the
    /// silent failure this exists to prevent. `dictool settings-validate`
    /// prints the same list for a file the user has not loaded yet.
    ///
    /// Two kinds:
    ///
    /// - an empty mapping, which `keymap_get` reports as unmapped;
    /// - several table entries naming the same key code. `u16::from_str` accepts
    ///   `10`, `010` and `+10` alike, and the raw TOML table is keyed on the
    ///   *string*, so those are three distinct entries resolving to one code.
    ///   Only one can win; `parse_keymap` picks it and this says which.
    ///
    /// `parse_keymap` records both as it makes each decision. Nothing here
    /// re-derives which entry won or which side was dropped, which is why the
    /// report and `keymap_get` cannot describe different files.
    pub fn keymap_warnings(&self) -> &[String] {
        &self.keymap_warnings
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

/// Default length variance weight (shared with FeatureWeights::default()).
pub const DEFAULT_LENGTH_VARIANCE_WEIGHT: i64 = 1000;
/// Default te-form kanji penalty (shared with FeatureWeights::default()).
pub const DEFAULT_TE_FORM_KANJI_PENALTY: i64 = 2000;
/// Default single-char kanji penalty (shared with FeatureWeights::default()).
pub const DEFAULT_SINGLE_CHAR_KANJI_PENALTY: i64 = 0;
/// Default structure cost transition cap.
pub const DEFAULT_STRUCTURE_COST_TRANSITION_CAP: i64 = 5000;

#[derive(Debug, Clone, Deserialize)]
pub struct RerankerSettings {
    pub length_variance_weight: i64,
    pub structure_cost_filter: i64,
    #[serde(default = "default_te_form_kanji_penalty")]
    pub te_form_kanji_penalty: i64,
    #[serde(default = "default_single_char_kanji_penalty")]
    pub single_char_kanji_penalty: i64,
    #[serde(default = "default_structure_cost_transition_cap")]
    pub structure_cost_transition_cap: i64,
}

fn default_te_form_kanji_penalty() -> i64 {
    DEFAULT_TE_FORM_KANJI_PENALTY
}

fn default_single_char_kanji_penalty() -> i64 {
    DEFAULT_SINGLE_CHAR_KANJI_PENALTY
}

fn default_structure_cost_transition_cap() -> i64 {
    DEFAULT_STRUCTURE_COST_TRANSITION_CAP
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
    "ctrl+shift+/".to_string()
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
///
/// The character is matched against `NSEvent.charactersIgnoringModifiers` on the
/// frontend, so it must correspond to the **base** (unshifted) key on the active
/// keyboard layout.  For example, on JIS keyboards `@` is a dedicated key, but on
/// US keyboards `@` requires Shift+2 and the base character is `2`.
fn parse_trigger_string(s: &str) -> Option<TriggerKey> {
    if s.is_empty() {
        return None;
    }

    let parts: Vec<&str> = s.split('+').collect();
    if parts.is_empty() {
        return None;
    }

    let char_part = parts.last()?.to_string();
    if char_part.is_empty() || char_part.chars().count() != 1 {
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
    let (parsed, warnings) = parse_keymap(&s.keymap)?;
    s.keymap_parsed = parsed;
    s.keymap_warnings = warnings;
    Ok(s)
}

/// The one place the `[keymap]` table's rules are applied: which entry a key
/// code resolves to, and which sides of it are real remaps. Both the map the
/// engine reads and the report of what was dropped come out of this single
/// pass, so no reader has to re-apply either rule and arrive somewhere else.
fn parse_keymap(
    raw: &HashMap<String, Vec<String>>,
) -> Result<(BTreeMap<u16, KeyRemap>, Vec<String>), SettingsError> {
    // Iterate the raw table in sorted key order. It is a `HashMap` keyed on the
    // *string*, and several strings can name one key code (`10` / `010` / `+10`
    // all parse to 10), so without a fixed order which of them wins would change
    // between launches. Sorted, the smallest spelling wins, every time.
    let mut raw_keys: Vec<&String> = raw.keys().collect();
    raw_keys.sort_unstable();
    let mut parsed: BTreeMap<u16, KeyRemap> = BTreeMap::new();
    let mut spellings: BTreeMap<u16, Vec<&str>> = BTreeMap::new();
    let mut warnings = Vec::new();
    for key_str in raw_keys {
        let values = &raw[key_str];
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
        let seen = spellings.entry(key_code).or_default();
        seen.push(key_str.as_str());
        if seen.len() > 1 {
            // A later spelling of a code that is already resolved. Its *sides*
            // are never consulted — which is why the empty-side rule below runs
            // only on the winner — but it has already been checked for
            // well-formedness above, like every other entry: whether the file
            // parses is not a question resolution order gets to answer.
            // Reported after the loop, once, naming the winner.
            continue;
        }
        // An empty mapping has nothing to insert, and the remap path would turn
        // it into a commit of "" — which inserts nothing yet still ends the
        // host's marked-text session (see `InputSession::
        // debug_assert_response_contract`). So it must not produce a remap: it
        // becomes `None` here, and that is the whole reason `keymap_get` can
        // report the side as unmapped.
        //
        // Ignored rather than rejected. This rule is newly tightened on a format
        // that already accepted such files, and `init_custom` is all-or-nothing
        // — refusing the file would revert every *other* custom value (costs,
        // history limits, snippet trigger, the working key mappings) to the
        // defaults over one bad entry — the disproportionate outcome CLAUDE.md's
        // 「オンディスク形式 (履歴 / user_dict / 設定) の変更は migration 必須」 is
        // about. Same treatment an unusable snippet body gets in
        // `SnippetStore::prefix_search`: drop what cannot be used, keep the
        // file, and say so. A *malformed* entry is a different question and
        // still rejects: this softens an empty value, not a broken file.
        let remap = KeyRemap {
            normal: usable_side(&values[0]),
            shifted: usable_side(&values[1]),
        };
        for (side, mapped) in [("normal", &remap.normal), ("shifted", &remap.shifted)] {
            if mapped.is_none() {
                warnings.push(format!(
                    "keymap.{key_str} {side} mapping is empty and will be ignored"
                ));
            }
        }
        parsed.insert(key_code, remap);
    }
    for (code, seen) in &spellings {
        if seen.len() > 1 {
            warnings.push(format!(
                "keymap has {} entries for key_code {code} ({}); only \"{}\" is used",
                seen.len(),
                seen.join(", "),
                seen[0],
            ));
        }
    }
    Ok((parsed, warnings))
}

/// The single definition of a usable mapping side. An empty one is not a remap
/// (see `parse_keymap`); expressing that as `None` at parse time is what lets
/// `keymap_get` stay a lookup instead of a search that can fall through.
fn usable_side(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
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
    check_non_negative!(reranker.te_form_kanji_penalty);
    check_non_negative!(reranker.single_char_kanji_penalty);
    check_non_negative!(reranker.structure_cost_transition_cap);

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

    // Validate snippet trigger (empty string intentionally disables)
    if !s.snippets.trigger.is_empty() && parse_trigger_string(&s.snippets.trigger).is_none() {
        return Err(SettingsError::InvalidValue {
            field: "snippets.trigger".to_string(),
            reason: "invalid trigger format (expected: [modifier+]...char)".to_string(),
        });
    }

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
        assert_eq!(s.cost.katakana_penalty, 150);
        assert_eq!(s.cost.pure_kanji_bonus, 1000);
        assert_eq!(s.cost.latin_penalty, 20000);
        assert_eq!(s.cost.unknown_word_cost, 10000);
        assert_eq!(s.reranker.length_variance_weight, 1000);
        assert_eq!(s.reranker.structure_cost_filter, 6000);
        assert_eq!(s.reranker.te_form_kanji_penalty, 2000);
        assert_eq!(s.reranker.single_char_kanji_penalty, 0);
        assert_eq!(s.reranker.structure_cost_transition_cap, 5000);
        assert_eq!(s.history.boost_per_use, 3000);
        assert_eq!(s.history.max_boost, 15000);
        assert!((s.history.half_life_hours - 168.0).abs() < f64::EPSILON);
        assert_eq!(s.history.max_unigrams, 10000);
        assert_eq!(s.history.max_bigrams, 10000);
        assert_eq!(s.candidates.nbest, 20);
        assert_eq!(s.candidates.max_results, 20);
        // Snippet defaults
        assert_eq!(s.snippets.trigger, "ctrl+shift+/");
        let trigger = s.snippet_trigger().unwrap();
        assert_eq!(trigger.char, "/");
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
structure_cost_filter = 6000

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
structure_cost_filter = 6000

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
structure_cost_filter = 6000

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
structure_cost_filter = 6000

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
structure_cost_filter = 6000

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

    /// Minimal valid settings TOML ending at an open `[keymap]` table, so a test
    /// can append the entries it wants to exercise.
    const KEYMAP_TEST_BASE: &str = r#"
[cost]
segment_penalty = 5000
mixed_script_bonus = 3000
katakana_penalty = 5000
pure_kanji_bonus = 1000
latin_penalty = 20000
unknown_word_cost = 10000

[reranker]
length_variance_weight = 2000
structure_cost_filter = 6000

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
"#;

    #[test]
    fn keymap_empty_mapping_is_unmapped_not_a_load_error() {
        // An empty mapping would reach the session as `Remapped { text: "" }`
        // and be committed as "", which inserts nothing but still ends the
        // host's marked-text session — so it must not produce a remap. It must
        // also not take the file down: `init_custom` is all-or-nothing, so
        // rejecting would revert every other custom value to the defaults over
        // one bad entry, on a format that already accepted such files.
        let base = KEYMAP_TEST_BASE;
        // (entry, expected normal, expected shifted)
        let cases = [
            ("10 = [\"\", \"}\"]", None, Some("}")),
            ("10 = [\"]\", \"\"]", Some("]"), None),
            ("10 = [\"\", \"\"]", None, None),
        ];
        for (entry, normal, shifted) in cases {
            let s = parse_settings_toml(&format!("{base}{entry}\n"))
                .unwrap_or_else(|e| panic!("{entry:?} must still load, got {e}"));
            assert_eq!(s.keymap_get(10, false), normal, "normal side of {entry:?}");
            assert_eq!(s.keymap_get(10, true), shifted, "shifted side of {entry:?}");
            // The rest of the file survives — that is the point of not rejecting.
            assert_eq!(s.candidates.nbest, 5);
            // ...and the drop is reportable rather than silent.
            assert!(
                s.keymap_warnings().iter().any(|w| w.contains("empty")),
                "{entry:?} should warn about the empty side",
            );
        }
    }

    #[test]
    fn keymap_duplicate_key_codes_resolve_deterministically() {
        // The raw table is keyed on the *string*, and `u16::from_str` accepts
        // `10`, `010` and `+10` alike — so these are three distinct entries for
        // one key code. Which one wins used to depend on `HashMap` iteration
        // order, i.e. it could differ between launches for an unchanged file.
        let base = KEYMAP_TEST_BASE;
        let toml =
            format!("{base}10 = [\"a\", \"A\"]\n010 = [\"b\", \"B\"]\n\"+10\" = [\"c\", \"C\"]\n");

        let first = parse_settings_toml(&toml).unwrap();
        assert_eq!(
            first.keymap_get(10, false),
            Some("c"),
            "\"+10\" sorts first"
        );
        assert_eq!(first.keymap_get(10, true), Some("C"));

        // Stable across repeated parses of the same input.
        for _ in 0..8 {
            let s = parse_settings_toml(&toml).unwrap();
            assert_eq!(s.keymap_get(10, false), first.keymap_get(10, false));
            assert_eq!(s.keymap_get(10, true), first.keymap_get(10, true));
        }

        let warnings = first.keymap_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("3 entries for key_code 10")),
            "the shadowing must be reported, got {warnings:?}",
        );
    }

    #[test]
    fn keymap_empty_side_of_the_winning_entry_stays_unmapped() {
        // The two rules meet here. `"+10"` sorts first, so it is the entry that
        // resolves key code 10 — including its empty normal side, which means
        // "not remapped". The shadowed `10 = ["b", "B"]` must not supply a
        // normal mapping in its place: `keymap_warnings` says only `"+10"` is
        // used, and `keymap_get` has to be describing that same file.
        let base = KEYMAP_TEST_BASE;
        let toml = format!("{base}\"+10\" = [\"\", \"A\"]\n10 = [\"b\", \"B\"]\n");
        let s = parse_settings_toml(&toml).unwrap();

        assert_eq!(
            s.keymap_get(10, false),
            None,
            "the winner's empty side must not fall through to the shadowed entry",
        );
        assert_eq!(
            s.keymap_get(10, true),
            Some("A"),
            "the winner supplies the side it does map",
        );

        let warnings = s.keymap_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("only \"+10\" is used")),
            "the shadowing must name the winner, got {warnings:?}",
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("keymap.+10 normal mapping is empty")),
            "the dropped side must be reported against the entry that owns it, got {warnings:?}",
        );
        // The shadowed entry is not read at all, so it produces no side report.
        assert!(
            !warnings.iter().any(|w| w.contains("keymap.10 ")),
            "a shadowed entry's sides are never consulted, got {warnings:?}",
        );
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
    fn parse_trigger_multi_char_returns_none() {
        assert!(parse_trigger_string("ctrl+enter").is_none());
        assert!(parse_trigger_string("ab").is_none());
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
