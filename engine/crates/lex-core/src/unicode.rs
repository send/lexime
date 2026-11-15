//! Character-level Unicode classification for Japanese text.

/// Check the full Hiragana block (U+3040..U+309F). This includes a few unassigned
/// codepoints (U+3040, U+3097-3098) but these never appear in IME input or
/// dictionary readings, so the simpler block-level check is preferred over an
/// exact range (U+3041..U+3096 + U+3099..U+309F) for clarity.
pub fn is_hiragana(c: char) -> bool {
    ('\u{3040}'..='\u{309F}').contains(&c)
}

/// Check the full Katakana block (U+30A0..U+30FF). Includes rarely-used symbols
/// (゠ U+30A0, ヿ U+30FF) but no unassigned codepoints.
pub fn is_katakana(c: char) -> bool {
    ('\u{30A0}'..='\u{30FF}').contains(&c)
}

pub fn is_kanji(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
        || ('\u{3400}'..='\u{4DBF}').contains(&c)
        || ('\u{20000}'..='\u{2A6DF}').contains(&c)
}

pub fn is_latin(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// Convert a hiragana string to katakana.
/// Non-hiragana characters (ー, ASCII, etc.) are passed through unchanged.
pub fn hiragana_to_katakana(s: &str) -> String {
    s.chars()
        .map(|c| {
            if is_hiragana(c) {
                char::from_u32(c as u32 + 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Check if a string is a valid hiragana reading.
///
/// Accepts hiragana characters (U+3040..U+309F) and the prolonged sound mark
/// ー (U+30FC, technically katakana) which commonly appears in readings like
/// "らーめん".
pub fn is_hiragana_reading(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| is_hiragana(c) || c == 'ー')
}

/// A candidate surface the symbol placement steps over (Mozc's
/// `SymbolRewriter` position rule, `symbol_rewriter.cc` `InsertCandidates`):
/// one kanji, all hiragana (ー allowed, as Mozc's `IsScriptType(HIRAGANA)`
/// allows it), or all katakana. Decided by the surface's script alone.
///
/// Differences from Mozc's `GetScriptType`, kept as they are because the
/// predicates above also price rerank's script feature: 々 is not a kanji
/// here, ゠ (U+30A0) is katakana here, and half-width katakana, the Ainu
/// extension and CJK compatibility ideographs count only in Mozc.
pub fn symbol_skip(s: &str) -> bool {
    let mut chars = s.chars();
    let one_kanji = matches!((chars.next(), chars.next()), (Some(c), None) if is_kanji(c));
    one_kanji || is_hiragana_reading(s) || (!s.is_empty() && s.chars().all(is_katakana))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hiragana_reading() {
        assert!(is_hiragana_reading("かんじ"));
        assert!(is_hiragana_reading("あ"));
        assert!(is_hiragana_reading("らーめん"));
        assert!(!is_hiragana_reading("カタカナ"));
        assert!(!is_hiragana_reading("abc"));
        assert!(!is_hiragana_reading(""));
    }

    #[test]
    fn test_hiragana_to_katakana() {
        assert_eq!(hiragana_to_katakana("きょうは"), "キョウハ");
        assert_eq!(hiragana_to_katakana("らーめん"), "ラーメン");
        assert_eq!(hiragana_to_katakana(""), "");
        assert_eq!(hiragana_to_katakana("abc"), "abc");
        assert_eq!(hiragana_to_katakana("カタカナ"), "カタカナ");
    }

    #[test]
    fn test_char_classification() {
        assert!(is_hiragana('あ'));
        assert!(!is_hiragana('ア'));
        assert!(is_katakana('ア'));
        assert!(is_katakana('ー'));
        assert!(!is_katakana('あ'));
        assert!(is_kanji('漢'));
        assert!(!is_kanji('あ'));
        assert!(is_latin('a'));
        assert!(!is_latin('あ'));
    }

    #[test]
    fn symbol_skip_steps_over_one_kanji_and_kana_runs() {
        for (s, want) in [
            ("空", true),
            ("漢字", false),
            // Not a kanji here (Mozc counts it): recorded difference.
            ("々", false),
            ("らーめん", true),
            ("ラーメン", true),
            // ・ is katakana-block, not hiragana: a mixed run is not skipped
            // (Mozc's IsScriptType(HIRAGANA) does not allow it either).
            ("ら・め", false),
            // ゠ is in lexime's katakana block, not Mozc's: recorded difference.
            ("ア゠イ", true),
            ("χ", false),
            ("", false),
        ] {
            assert_eq!(symbol_skip(s), want, "{s}");
        }
    }
}
