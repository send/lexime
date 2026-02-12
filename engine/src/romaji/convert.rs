use super::trie::{RomajiTrie, TrieLookupResult};

pub struct RomajiConvertResult {
    pub composed_kana: String,
    pub pending_romaji: String,
}

/// Map kana vowel chars to their romaji equivalents for collapse_latin_kana.
fn kana_vowel_to_romaji(ch: char) -> Option<char> {
    match ch {
        'あ' => Some('a'),
        'い' => Some('i'),
        'う' => Some('u'),
        'え' => Some('e'),
        'お' => Some('o'),
        _ => None,
    }
}

fn is_vowel(ch: char) -> bool {
    matches!(ch, 'a' | 'i' | 'u' | 'e' | 'o')
}

/// Collapse sequences of latin consonant(s) + kana vowel into a single kana.
/// e.g. "kあ" → "か", "shあ" → "しゃ"
fn collapse_latin_kana(input: &str, trie: &RomajiTrie) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if ch.is_ascii_lowercase() {
            // Collect consecutive ASCII lowercase chars
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_lowercase() {
                j += 1;
            }

            // Check if followed by a kana vowel
            if j < chars.len() {
                if let Some(vowel) = kana_vowel_to_romaji(chars[j]) {
                    let latin: String = chars[i..j].iter().collect();
                    let candidate = format!("{latin}{vowel}");
                    match trie.lookup(&candidate) {
                        TrieLookupResult::Exact(ref kana)
                        | TrieLookupResult::ExactAndPrefix(ref kana) => {
                            result.push_str(kana);
                            i = j + 1;
                            continue;
                        }
                        _ => {}
                    }
                }
            }

            result.push(ch);
            i += 1;
        } else {
            result.push(ch);
            i += 1;
        }
    }

    result
}

/// Convert pending romaji to kana, mirroring the Swift `drainPendingRomaji` logic.
///
/// When `force` is true, ambiguous sequences are resolved immediately
/// (e.g. trailing "n" becomes "ん").
pub fn convert_romaji(
    composed_kana: &str,
    pending_romaji: &str,
    force: bool,
) -> RomajiConvertResult {
    let trie = RomajiTrie::global();
    let mut composed = composed_kana.to_string();
    let mut pending = pending_romaji.to_string();

    let mut changed = true;
    while !pending.is_empty() && changed {
        changed = false;
        let result = trie.lookup(&pending);

        match result {
            TrieLookupResult::Exact(kana) => {
                composed.push_str(&kana);
                pending.clear();
                changed = true;
            }

            TrieLookupResult::ExactAndPrefix(kana) => {
                if force {
                    composed.push_str(&kana);
                    pending.clear();
                    changed = true;
                }
            }

            TrieLookupResult::Prefix => {
                if !force {
                    break;
                }
                // force: fall through to None logic
                handle_no_match(trie, &mut composed, &mut pending, force, &mut changed);
            }

            TrieLookupResult::None => {
                handle_no_match(trie, &mut composed, &mut pending, force, &mut changed);
            }
        }
    }

    // Collapse latin consonant + kana vowel sequences
    if composed.chars().any(|c| c.is_ascii_lowercase()) {
        composed = collapse_latin_kana(&composed, trie);
    }

    RomajiConvertResult {
        composed_kana: composed,
        pending_romaji: pending,
    }
}

/// Handle the case where `pending` has no full match: try sub-prefix,
/// sokuon/hatsuon detection, or force-drain.
fn handle_no_match(
    trie: &RomajiTrie,
    composed: &mut String,
    pending: &mut String,
    force: bool,
    changed: &mut bool,
) {
    // Try sub-prefixes from longest to shortest.
    //
    // Note: ExactAndPrefix is consumed here regardless of `force`, unlike the
    // main loop which defers ExactAndPrefix when force=false (to allow longer
    // matches from subsequent keystrokes).  This is intentional — we only reach
    // handle_no_match when the FULL pending already failed to match, so there is
    // no longer sequence to wait for.  Refusing to consume ExactAndPrefix here
    // would leave pending permanently stuck.
    let mut found = false;
    let pending_bytes = pending.as_bytes();
    for len in (1..pending_bytes.len()).rev() {
        let sub = &pending[..len];
        match trie.lookup(sub) {
            TrieLookupResult::Exact(kana) | TrieLookupResult::ExactAndPrefix(kana) => {
                composed.push_str(&kana);
                *pending = pending[len..].to_string();
                found = true;
                *changed = true;
                break;
            }
            _ => {}
        }
    }

    if !found {
        let chars: Vec<char> = pending.chars().collect();
        if chars.len() >= 2 {
            let first = chars[0];
            let second = chars[1];
            if first == second && first != 'n' && !is_vowel(first) {
                // Sokuon (っ): doubled consonant
                composed.push('っ');
                *pending = pending.chars().skip(1).collect();
                *changed = true;
            } else if first == 'n' && !is_vowel(second) && second != 'n' && second != 'y' {
                // Hatsuon (ん): n before non-vowel, non-n, non-y
                composed.push('ん');
                *pending = pending.chars().skip(1).collect();
                *changed = true;
            } else if force {
                // Force: drain first char as-is
                let c = pending.remove(0);
                composed.push(c);
                *changed = true;
            }
            // else: leave in pending (changed stays false → loop exits)
        } else {
            // Single character remaining
            if *pending == "n" {
                if force {
                    composed.push('ん');
                }
                // When not forced, "n" stays pending (could be prefix of "na", etc.)
            } else {
                // R1 fix: preserve unrecognized single chars in composedKana
                composed.push_str(pending);
            }
            pending.clear();
            *changed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(kana: &str, pending: &str, force: bool) -> RomajiConvertResult {
        convert_romaji(kana, pending, force)
    }

    #[test]
    fn test_basic_ka() {
        let r = convert("", "ka", false);
        assert_eq!(r.composed_kana, "か");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_sokuon_kk() {
        let r = convert("", "kk", false);
        assert_eq!(r.composed_kana, "っ");
        assert_eq!(r.pending_romaji, "k");
    }

    #[test]
    fn test_hatsuon_nk() {
        let r = convert("", "nk", false);
        assert_eq!(r.composed_kana, "ん");
        assert_eq!(r.pending_romaji, "k");
    }

    #[test]
    fn test_n_force() {
        let r = convert("", "n", true);
        assert_eq!(r.composed_kana, "ん");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_n_no_force() {
        let r = convert("", "n", false);
        assert_eq!(r.composed_kana, "");
        assert_eq!(r.pending_romaji, "n");
    }

    #[test]
    fn test_consecutive_kakiku() {
        let r = convert("", "kakiku", false);
        assert_eq!(r.composed_kana, "かきく");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_q_prefix_stays_pending() {
        let r = convert("", "q", false);
        assert_eq!(r.composed_kana, "");
        assert_eq!(r.pending_romaji, "q");
    }

    #[test]
    fn test_shi() {
        let r = convert("", "shi", false);
        assert_eq!(r.composed_kana, "し");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_existing_composed_preserved() {
        let r = convert("あ", "ka", false);
        assert_eq!(r.composed_kana, "あか");
    }

    #[test]
    fn test_youon_sha() {
        let r = convert("", "sha", false);
        assert_eq!(r.composed_kana, "しゃ");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_mixed_kyouha() {
        let r = convert("", "kyouha", false);
        assert_eq!(r.composed_kana, "きょうは");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_sokuon_kka() {
        let r = convert("", "kka", false);
        assert_eq!(r.composed_kana, "っか");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_collapse_k_a() {
        let r = convert("kあ", "", false);
        assert_eq!(r.composed_kana, "か");
    }

    #[test]
    fn test_collapse_mid() {
        let r = convert("あkい", "", false);
        assert_eq!(r.composed_kana, "あき");
    }

    #[test]
    fn test_collapse_multi_latin() {
        let r = convert("shあ", "", false);
        assert_eq!(r.composed_kana, "しゃ");
    }

    #[test]
    fn test_no_collapse_non_vowel() {
        let r = convert("kが", "", false);
        assert_eq!(r.composed_kana, "kが");
    }

    #[test]
    fn test_invalid_chy_no_force() {
        let r = convert("", "chy", false);
        assert_eq!(r.composed_kana, "");
        assert_eq!(r.pending_romaji, "chy");
    }

    #[test]
    fn test_invalid_chy_force() {
        let r = convert("", "chy", true);
        assert_eq!(r.composed_kana, "chy");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_backspace_recovery_chi() {
        let r = convert("", "chi", false);
        assert_eq!(r.composed_kana, "ち");
        assert_eq!(r.pending_romaji, "");
    }

    #[test]
    fn test_tc_no_force() {
        let r = convert("", "tc", false);
        assert_eq!(r.composed_kana, "");
        assert_eq!(r.pending_romaji, "tc");
    }
}
