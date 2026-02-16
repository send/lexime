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
}
