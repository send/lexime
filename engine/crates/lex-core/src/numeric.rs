//! Japanese kana-to-number conversion.
//!
//! Parses hiragana number words (いち, にじゅうさん, さんびゃくよんじゅうご, etc.)
//! into numeric values and formats them as half-width or full-width digits.
//! Supports rendaku (連濁) variants and values up to 兆 (10^12).

/// Parse a hiragana number string into a numeric value.
///
/// Returns `None` if the input is not a valid Japanese number expression.
pub fn parse_japanese_number(kana: &str) -> Option<u64> {
    let first = kana.chars().next()?;
    if !matches!(
        first,
        'い' | 'に'
            | 'さ'
            | 'し'
            | 'よ'
            | 'ご'
            | 'ろ'
            | 'な'
            | 'は'
            | 'き'
            | 'く'
            | 'ぜ'
            | 'れ'
            | 'じ'
            | 'ひ'
            | 'せ'
            | 'ま'
            | 'お'
            | 'ち'
    ) {
        return None;
    }

    let mut rest = kana;
    let mut result: u64 = 0;
    let mut group = parse_group(&mut rest);

    // Large units: ちょう(10^12), おく(10^8), まん(10^4)
    for (unit_kana, unit_val) in &[
        ("ちょう", 1_000_000_000_000u64),
        ("おく", 100_000_000),
        ("まん", 10_000),
    ] {
        if let Some(pos) = rest.find(unit_kana) {
            // Everything before the unit should have been consumed into group
            if pos != 0 {
                return None;
            }
            rest = &rest[unit_kana.len()..];
            // If group is 0 before a large unit, it means the unit stands alone (e.g. まん = 10000)
            if group == 0 {
                group = 1;
            }
            result += group * unit_val;
            group = parse_group(&mut rest);
        }
    }

    result += group;

    if !rest.is_empty() {
        return None;
    }
    if result == 0 && kana != "ぜろ" && kana != "れい" {
        return None;
    }

    Some(result)
}

/// Parse a group value (< 10000) from the front of `rest`, advancing the slice.
fn parse_group(rest: &mut &str) -> u64 {
    let mut value: u64 = 0;

    // せん(1000)
    value += parse_unit(rest, 1000);
    // ひゃく(100)
    value += parse_unit(rest, 100);
    // じゅう(10)
    value += parse_unit(rest, 10);
    // Trailing digit
    if let Some((d, len)) = consume_digit(rest) {
        *rest = &rest[len..];
        value += d;
    }

    value
}

/// Parse [digit] + unit from `rest`. Returns the contribution (digit * unit_val).
fn parse_unit(rest: &mut &str, unit_val: u64) -> u64 {
    // Try digit + unit
    let saved = *rest;
    if let Some((d, dlen)) = consume_digit_or_rendaku_prefix(rest, unit_val) {
        let after_digit = &saved[dlen..];
        if let Some(ulen) = consume_unit_kana(after_digit, unit_val) {
            *rest = &after_digit[ulen..];
            return d * unit_val;
        }
        // No unit followed — restore
        // (don't restore if it was a rendaku prefix, those are only valid before units)
    }

    // Try bare unit (e.g. ひゃく = 100, じゅう = 10, せん = 1000)
    *rest = saved;
    if let Some(ulen) = consume_unit_kana(rest, unit_val) {
        *rest = &rest[ulen..];
        return unit_val;
    }

    0
}

/// Try to consume a digit (1-9) or a rendaku prefix from the front of `s`.
/// Returns (digit_value, byte_length) if found.
fn consume_digit_or_rendaku_prefix(s: &str, unit_val: u64) -> Option<(u64, usize)> {
    // Rendaku prefixes (only valid before specific units)
    match unit_val {
        100 => {
            // ろっぴゃく(600), はっぴゃく(800)
            if s.starts_with("ろっ") {
                return Some((6, "ろっ".len()));
            }
            if s.starts_with("はっ") {
                return Some((8, "はっ".len()));
            }
        }
        1000 => {
            // はっせん(8000)
            if s.starts_with("はっ") {
                return Some((8, "はっ".len()));
            }
        }
        10 => {
            // じっ as prefix for じゅう (じっ = 10, used in じっかい etc., but also standalone)
            // Not a digit prefix — handled as a unit variant
        }
        _ => {}
    }

    // Standard digits
    consume_digit(s)
}

/// Try to consume a standard digit (0-9) from the front of `s`.
fn consume_digit(s: &str) -> Option<(u64, usize)> {
    let table: &[(&str, u64)] = &[
        ("きゅう", 9),
        ("しち", 7),
        ("よん", 4),
        ("はち", 8),
        ("ろく", 6),
        ("なな", 7),
        ("いち", 1),
        ("さん", 3),
        ("ぜろ", 0),
        ("れい", 0),
        ("に", 2),
        ("し", 4),
        ("ご", 5),
        ("く", 9),
    ];
    for &(kana, val) in table {
        if s.starts_with(kana) {
            return Some((val, kana.len()));
        }
    }
    None
}

/// Try to consume a unit kana from the front of `s`. Returns byte length if matched.
fn consume_unit_kana(s: &str, unit_val: u64) -> Option<usize> {
    let variants: &[&str] = match unit_val {
        10 => &["じゅう", "じゅっ", "じっ"],
        100 => &["ひゃく", "びゃく", "ぴゃく"],
        1000 => &["せん", "ぜん"],
        _ => return None,
    };
    for &v in variants {
        if s.starts_with(v) {
            return Some(v.len());
        }
    }
    None
}

/// Format a number as half-width Arabic digits.
pub fn to_halfwidth(n: u64) -> String {
    n.to_string()
}

/// Format a number as full-width Arabic digits.
pub fn to_fullwidth(n: u64) -> String {
    n.to_string()
        .chars()
        .map(|c| char::from_u32(c as u32 - '0' as u32 + '０' as u32).unwrap_or(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_digits() {
        assert_eq!(parse_japanese_number("ぜろ"), Some(0));
        assert_eq!(parse_japanese_number("れい"), Some(0));
        assert_eq!(parse_japanese_number("いち"), Some(1));
        assert_eq!(parse_japanese_number("に"), Some(2));
        assert_eq!(parse_japanese_number("さん"), Some(3));
        assert_eq!(parse_japanese_number("し"), Some(4));
        assert_eq!(parse_japanese_number("よん"), Some(4));
        assert_eq!(parse_japanese_number("ご"), Some(5));
        assert_eq!(parse_japanese_number("ろく"), Some(6));
        assert_eq!(parse_japanese_number("しち"), Some(7));
        assert_eq!(parse_japanese_number("なな"), Some(7));
        assert_eq!(parse_japanese_number("はち"), Some(8));
        assert_eq!(parse_japanese_number("きゅう"), Some(9));
        assert_eq!(parse_japanese_number("く"), Some(9));
    }

    #[test]
    fn test_tens() {
        assert_eq!(parse_japanese_number("じゅう"), Some(10));
        assert_eq!(parse_japanese_number("にじゅう"), Some(20));
        assert_eq!(parse_japanese_number("にじゅうさん"), Some(23));
        assert_eq!(parse_japanese_number("さんじゅう"), Some(30));
        assert_eq!(parse_japanese_number("きゅうじゅうきゅう"), Some(99));
    }

    #[test]
    fn test_hundreds() {
        assert_eq!(parse_japanese_number("ひゃく"), Some(100));
        assert_eq!(parse_japanese_number("にひゃく"), Some(200));
        assert_eq!(parse_japanese_number("さんびゃく"), Some(300));
        assert_eq!(parse_japanese_number("ろっぴゃく"), Some(600));
        assert_eq!(parse_japanese_number("はっぴゃく"), Some(800));
    }

    #[test]
    fn test_thousands() {
        assert_eq!(parse_japanese_number("せん"), Some(1000));
        assert_eq!(parse_japanese_number("さんぜん"), Some(3000));
        assert_eq!(parse_japanese_number("はっせん"), Some(8000));
    }

    #[test]
    fn test_compound() {
        assert_eq!(parse_japanese_number("さんびゃくよんじゅうご"), Some(345));
        assert_eq!(
            parse_japanese_number("いっせんにひゃくさんじゅうよん"),
            None // いっせん not supported (would need いっ rendaku prefix for せん)
        );
        assert_eq!(
            parse_japanese_number("せんにひゃくさんじゅうよん"),
            Some(1234)
        );
    }

    #[test]
    fn test_large_units() {
        assert_eq!(parse_japanese_number("いちまん"), Some(10_000));
        assert_eq!(parse_japanese_number("じゅうまん"), Some(100_000));
        assert_eq!(parse_japanese_number("いちおく"), Some(100_000_000));
        assert_eq!(parse_japanese_number("いっちょう"), None); // いっちょう not supported
        assert_eq!(parse_japanese_number("いちちょう"), Some(1_000_000_000_000));
    }

    #[test]
    fn test_complex() {
        // 12345 = いちまんにせんさんびゃくよんじゅうご
        assert_eq!(
            parse_japanese_number("いちまんにせんさんびゃくよんじゅうご"),
            Some(12345)
        );
    }

    #[test]
    fn test_non_numeric() {
        assert_eq!(parse_japanese_number("こんにちは"), None);
        assert_eq!(parse_japanese_number("きょう"), None);
        assert_eq!(parse_japanese_number("あ"), None);
        assert_eq!(parse_japanese_number(""), None);
    }

    #[test]
    fn test_halfwidth() {
        assert_eq!(to_halfwidth(0), "0");
        assert_eq!(to_halfwidth(123), "123");
        assert_eq!(to_halfwidth(10000), "10000");
    }

    #[test]
    fn test_fullwidth() {
        assert_eq!(to_fullwidth(0), "０");
        assert_eq!(to_fullwidth(123), "１２３");
        assert_eq!(to_fullwidth(10000), "１００００");
    }
}
