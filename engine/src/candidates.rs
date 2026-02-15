use std::collections::HashSet;

use tracing::{debug, debug_span};

use crate::converter::{convert_nbest, ConvertedSegment};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::{DictEntry, Dictionary, TrieDictionary};
use crate::user_history::UserHistory;

/// Alternative forms for punctuation characters.
/// When the reading is a punctuation kana, we show the original + these alternatives.
static PUNCTUATION_ALTERNATIVES: &[(&str, &[&str])] = &[
    ("。", &["．", "."]),
    ("、", &["，", ","]),
    ("？", &["?"]),
    ("！", &["!"]),
    ("「", &["｢", "["]),
    ("」", &["｣", "]"]),
    ("・", &["／", "/"]),
    ("〜", &["~"]),
];

/// Result of unified candidate generation.
pub struct CandidateResponse {
    /// Candidate surfaces for display (ordered, deduplicated).
    pub surfaces: Vec<String>,
    /// N-best paths for segment-level learning.
    pub paths: Vec<Vec<ConvertedSegment>>,
}

/// Look up punctuation alternatives for a reading.
fn punctuation_alternatives(reading: &str) -> Option<&'static [&'static str]> {
    PUNCTUATION_ALTERNATIVES
        .iter()
        .find(|&&(k, _)| k == reading)
        .map(|&(_, v)| v)
}

/// Generate punctuation candidates: learned predictions first, then default + alternatives.
fn generate_punctuation_candidates(
    dict: &TrieDictionary,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();

    // Learned predictions first
    if let Some(h) = history {
        let fetch_limit = max_results.max(200);
        let mut ranked = dict.predict_ranked(reading, fetch_limit, 1000);
        ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
            let boost_a = h.unigram_boost(r_a, &e_a.surface);
            let boost_b = h.unigram_boost(r_b, &e_b.surface);
            boost_b.cmp(&boost_a).then(e_a.cost.cmp(&e_b.cost))
        });
        ranked.truncate(max_results);
        for (_, entry) in &ranked {
            if seen.insert(entry.surface.clone()) {
                surfaces.push(entry.surface.clone());
            }
        }
    }

    // Reading itself
    if seen.insert(reading.to_string()) {
        surfaces.push(reading.to_string());
    }

    // Alternatives
    if let Some(alts) = punctuation_alternatives(reading) {
        for &alt in alts {
            if seen.insert(alt.to_string()) {
                surfaces.push(alt.to_string());
            }
        }
    }

    CandidateResponse {
        surfaces,
        paths: Vec::new(),
    }
}

/// Generate candidates for normal (non-punctuation) input.
fn generate_normal_candidates(
    dict: &TrieDictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();

    // 1. N-best Viterbi conversion (pure statistical, no history bias).
    //    History influence is applied at the candidate level (kana promotion,
    //    predictions, lookup reordering) rather than distorting Viterbi paths,
    //    which avoids stale history overriding the statistically best results.
    let nbest = 5usize;
    let paths = convert_nbest(dict, conn, reading, nbest);

    let mut nbest_paths = Vec::new();
    for path in &paths {
        let joined: String = path.iter().map(|s| s.surface.as_str()).collect();
        if !joined.is_empty() && seen.insert(joined.clone()) {
            surfaces.push(joined);
        }
        nbest_paths.push(path.clone());
    }

    // 2. Reading (kana) — position depends on history boost.
    //    If the user previously selected the hiragana form, promote it to
    //    position 0 (inline preview) so it becomes the default candidate.
    //    The kana may already exist in N-best (via fallback nodes); if so, move it.
    let kana_boost = history.map_or(0, |h| h.unigram_boost(reading, reading));
    let kana_existing_pos = surfaces.iter().position(|s| s == reading);
    if kana_boost > 0 {
        match kana_existing_pos {
            Some(0) => {} // already at top
            Some(pos) => {
                surfaces.remove(pos);
                surfaces.insert(0, reading.to_string());
            }
            None => {
                seen.insert(reading.to_string());
                surfaces.insert(0, reading.to_string());
            }
        }
    } else if kana_existing_pos.is_none() {
        seen.insert(reading.to_string());
        surfaces.push(reading.to_string());
    }

    // 3. Predictions (ranked by history if available)
    let fetch_limit = if history.is_some() {
        max_results.max(200)
    } else {
        max_results
    };
    let mut ranked = dict.predict_ranked(reading, fetch_limit, 1000);
    if let Some(h) = history {
        ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
            let boost_a = h.unigram_boost(r_a, &e_a.surface);
            let boost_b = h.unigram_boost(r_b, &e_b.surface);
            boost_b.cmp(&boost_a).then(e_a.cost.cmp(&e_b.cost))
        });
        ranked.truncate(max_results);
    }
    for (_, entry) in &ranked {
        if !entry.surface.is_empty() && seen.insert(entry.surface.clone()) {
            surfaces.push(entry.surface.clone());
        }
    }

    // 5. Dictionary lookup
    let lookup_entries: Vec<&DictEntry> = if let Some(h) = history {
        if let Some(entries) = dict.lookup(reading) {
            let reordered = h.reorder_candidates(reading, entries);
            // We need owned entries; collect surfaces
            for entry in &reordered {
                if seen.insert(entry.surface.clone()) {
                    surfaces.push(entry.surface.clone());
                }
            }
            Vec::new() // already processed
        } else {
            Vec::new()
        }
    } else if let Some(entries) = dict.lookup(reading) {
        entries.iter().collect()
    } else {
        Vec::new()
    };

    for entry in &lookup_entries {
        if seen.insert(entry.surface.clone()) {
            surfaces.push(entry.surface.clone());
        }
    }

    CandidateResponse {
        surfaces,
        paths: nbest_paths,
    }
}

/// Unified candidate generation: handles both punctuation and normal input.
pub fn generate_candidates(
    dict: &TrieDictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let _span = debug_span!("generate_candidates", reading, max_results).entered();
    if reading.is_empty() {
        return CandidateResponse {
            surfaces: Vec::new(),
            paths: Vec::new(),
        };
    }

    let resp = if punctuation_alternatives(reading).is_some() {
        generate_punctuation_candidates(dict, history, reading, max_results)
    } else {
        generate_normal_candidates(dict, conn, history, reading, max_results)
    };
    debug!(
        surface_count = resp.surfaces.len(),
        path_count = resp.paths.len()
    );
    resp
}

/// Chain bigram successors from a starting surface to build multi-word phrases.
/// Returns the chained phrase (start_surface + successors) or None if no successors found.
/// Detects cycles (repeated surfaces) and stops chaining to avoid garbage output.
fn chain_bigram_phrase(
    history: &UserHistory,
    start_surface: &str,
    max_chain: usize,
) -> Option<String> {
    let mut result = start_surface.to_string();
    let mut current_surface = start_surface.to_string();
    let mut visited = HashSet::new();
    visited.insert(current_surface.clone());
    let mut extended = false;

    for _ in 0..max_chain {
        let successors = history.bigram_successors(&current_surface);
        if let Some((_, next_surface, _)) = successors.first() {
            if !visited.insert(next_surface.clone()) {
                break; // cycle detected
            }
            result.push_str(next_surface);
            current_surface.clone_from(next_surface);
            extended = true;
        } else {
            break;
        }
    }

    if extended {
        Some(result)
    } else {
        None
    }
}

/// Generate prediction candidates with bigram chaining (Copilot-like completions).
/// Uses Viterbi N-best as the base, then chains bigram successors from history
/// to produce progressively longer multi-word phrases.
pub fn generate_prediction_candidates(
    dict: &TrieDictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let _span = debug_span!("generate_prediction_candidates", reading, max_results).entered();
    if reading.is_empty() {
        return CandidateResponse {
            surfaces: Vec::new(),
            paths: Vec::new(),
        };
    }

    // Punctuation falls back to standard punctuation candidates
    if punctuation_alternatives(reading).is_some() {
        return generate_punctuation_candidates(dict, history, reading, max_results);
    }

    // Get base candidates (same Viterbi N-best + predictions as Standard)
    let base = generate_normal_candidates(dict, conn, history, reading, max_results);

    let Some(h) = history else {
        // No history → can't chain, return standard candidates
        debug!(surface_count = base.surfaces.len(), "no history, fallback");
        return base;
    };

    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();
    let max_chain = 5;

    // Build chained phrases from N-best paths (use last segment surface for chaining)
    let mut chained_phrases: Vec<(String, usize)> = Vec::new(); // (phrase, length)
    let mut chained_starts: HashSet<String> = HashSet::new(); // track chain start surfaces

    for path in &base.paths {
        if path.is_empty() {
            continue;
        }
        let last_surface = &path.last().unwrap().surface;
        let joined: String = path.iter().map(|s| s.surface.as_str()).collect();

        chained_starts.insert(joined.clone());
        if let Some(chained) = chain_bigram_phrase(h, last_surface, max_chain) {
            let full = format!("{}{}", joined, &chained[last_surface.len()..]);
            let chain_len = full.chars().count();
            if full != joined {
                chained_phrases.push((full, chain_len));
            }
        }
    }

    // Also try chaining from base candidate surfaces not already covered by paths
    for surface in &base.surfaces {
        if chained_starts.contains(surface) {
            continue;
        }
        if let Some(chained) = chain_bigram_phrase(h, surface, max_chain) {
            let chain_len = chained.chars().count();
            chained_phrases.push((chained, chain_len));
        }
    }

    // Sort chained phrases by length descending (longest first = most Copilot-like)
    chained_phrases.sort_by(|a, b| b.1.cmp(&a.1));

    // Add chained phrases first (longest completions)
    for (phrase, _) in &chained_phrases {
        if seen.insert(phrase.clone()) {
            surfaces.push(phrase.clone());
        }
    }

    // Then add base candidates
    for s in &base.surfaces {
        if seen.insert(s.clone()) {
            surfaces.push(s.clone());
        }
    }

    surfaces.truncate(max_results);

    debug!(
        surface_count = surfaces.len(),
        chained_count = chained_phrases.len(),
    );
    CandidateResponse {
        surfaces,
        paths: base.paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dict() -> TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 3000,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "京".to_string(),
                        cost: 5000,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "。".to_string(),
                vec![DictEntry {
                    surface: "。".to_string(),
                    cost: 1000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    fn test_punctuation_candidates() {
        let dict = make_dict();
        let resp = generate_candidates(&dict, None, None, "。", 9);
        assert!(resp.surfaces.contains(&"。".to_string()));
        assert!(resp.surfaces.contains(&"．".to_string()));
        assert!(resp.surfaces.contains(&".".to_string()));
        assert!(resp.paths.is_empty());
    }

    #[test]
    fn test_normal_candidates() {
        let dict = make_dict();
        let resp = generate_candidates(&dict, None, None, "きょう", 9);
        // Viterbi #1 should be first (conversion result, not kana)
        // Kana should still be present
        assert!(resp.surfaces.contains(&"きょう".to_string()));
        assert!(resp.surfaces.contains(&"今日".to_string()));
        assert!(resp.surfaces.contains(&"京".to_string()));
        // N-best paths should be non-empty
        assert!(!resp.paths.is_empty());
    }

    #[test]
    fn test_empty_reading() {
        let dict = make_dict();
        let resp = generate_candidates(&dict, None, None, "", 9);
        assert!(resp.surfaces.is_empty());
        assert!(resp.paths.is_empty());
    }

    #[test]
    fn test_no_duplicates() {
        let dict = make_dict();
        let resp = generate_candidates(&dict, None, None, "きょう", 20);
        let unique: HashSet<&String> = resp.surfaces.iter().collect();
        assert_eq!(
            unique.len(),
            resp.surfaces.len(),
            "candidates should be deduplicated"
        );
    }

    #[test]
    fn test_punctuation_mode_detected() {
        assert!(punctuation_alternatives("。").is_some());
        assert!(punctuation_alternatives("、").is_some());
        assert!(punctuation_alternatives("？").is_some());
        assert!(punctuation_alternatives("きょう").is_none());
    }

    #[test]
    fn test_kana_promoted_by_history() {
        let dict = make_dict();
        let mut h = UserHistory::new();
        // Record hiragana selection: user chose "きょう" (kana) for reading "きょう"
        h.record(&[("きょう".into(), "きょう".into())]);

        let resp = generate_candidates(&dict, None, Some(&h), "きょう", 9);
        // Kana "きょう" should appear at position 1 (after Viterbi #1, before other N-best)
        assert_eq!(
            resp.surfaces[0], "きょう",
            "kana should be promoted to position 0 (inline preview)"
        );
    }

    #[test]
    fn test_prediction_bigram_chaining() {
        let dict = make_dict();
        let mut h = UserHistory::new();
        // Record a sentence: 今日は → bigrams: 今日→は
        h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);

        let resp = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20);
        // Should contain chained phrase "今日は"
        assert!(
            resp.surfaces.contains(&"今日は".to_string()),
            "should contain chained phrase '今日は', got: {:?}",
            resp.surfaces,
        );
        // Chained phrase should appear before unchained base candidates
        let chained_pos = resp.surfaces.iter().position(|s| s == "今日は").unwrap();
        let base_pos = resp.surfaces.iter().position(|s| s == "今日");
        if let Some(bp) = base_pos {
            assert!(
                chained_pos < bp,
                "chained phrase should appear before base candidate"
            );
        }
    }

    #[test]
    fn test_prediction_no_chaining_without_history() {
        let dict = make_dict();
        let resp = generate_prediction_candidates(&dict, None, None, "きょう", 20);
        // Without history, should behave like standard candidates
        assert!(resp.surfaces.contains(&"今日".to_string()));
        assert!(resp.surfaces.contains(&"きょう".to_string()));
    }

    #[test]
    fn test_prediction_multi_word_chain() {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "いい".to_string(),
                vec![DictEntry {
                    surface: "良い".to_string(),
                    cost: 3500,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "てんき".to_string(),
                vec![DictEntry {
                    surface: "天気".to_string(),
                    cost: 4000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        let dict = TrieDictionary::from_entries(entries);
        let mut h = UserHistory::new();
        // Record a full sentence: 今日は良い天気
        h.record(&[
            ("きょう".into(), "今日".into()),
            ("は".into(), "は".into()),
            ("いい".into(), "良い".into()),
            ("てんき".into(), "天気".into()),
        ]);

        let resp = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20);
        // Should contain the full chained phrase
        assert!(
            resp.surfaces.contains(&"今日は良い天気".to_string()),
            "should contain multi-word chain '今日は良い天気', got: {:?}",
            resp.surfaces,
        );
    }

    #[test]
    fn test_chain_bigram_phrase_basic() {
        let mut h = UserHistory::new();
        h.record(&[
            ("きょう".into(), "今日".into()),
            ("は".into(), "は".into()),
            ("いい".into(), "良い".into()),
        ]);
        let result = chain_bigram_phrase(&h, "今日", 5);
        assert_eq!(result.as_deref(), Some("今日は良い"));
    }

    #[test]
    fn test_chain_bigram_phrase_no_successors() {
        let h = UserHistory::new();
        assert!(chain_bigram_phrase(&h, "今日", 5).is_none());
    }

    #[test]
    fn test_chain_bigram_phrase_cycle_detection() {
        let mut h = UserHistory::new();
        // Create a cycle: A→B→A
        h.record(&[("あ".into(), "A".into()), ("び".into(), "B".into())]);
        h.record(&[("び".into(), "B".into()), ("あ".into(), "A".into())]);

        let result = chain_bigram_phrase(&h, "A", 10);
        // Should chain A→B then stop (A already visited)
        assert_eq!(result.as_deref(), Some("AB"));
    }

    #[test]
    fn test_chain_bigram_phrase_self_loop() {
        let mut h = UserHistory::new();
        // Self-loop: は→は
        h.record(&[("は".into(), "は".into()), ("は".into(), "は".into())]);

        let result = chain_bigram_phrase(&h, "は", 10);
        // Should not chain at all (first successor is "は" which is already visited)
        assert!(result.is_none());
    }

    #[test]
    fn test_prediction_cycle_produces_no_garbage() {
        let dict = make_dict();
        let mut h = UserHistory::new();
        // Create a cycle: は→は (self-loop)
        h.record(&[("は".into(), "は".into()), ("は".into(), "は".into())]);

        let resp = generate_prediction_candidates(&dict, None, Some(&h), "は", 20);
        // No candidate should contain repeated "ははは..." garbage
        for surface in &resp.surfaces {
            assert!(
                !surface.contains("はは"),
                "should not contain repeated garbage: {}",
                surface,
            );
        }
    }

    #[test]
    fn test_kana_not_promoted_without_history() {
        let dict = make_dict();
        let resp = generate_candidates(&dict, None, None, "きょう", 9);
        // Without history, kana should NOT be at position 1
        // (it should be after all N-best paths)
        if resp.surfaces.len() >= 2 {
            // Position 0 is Viterbi #1 (likely kanji), kana comes after N-best
            let kana_pos = resp.surfaces.iter().position(|s| s == "きょう").unwrap();
            assert!(
                kana_pos > 0,
                "kana should not be at position 0 without history"
            );
        }
    }
}
