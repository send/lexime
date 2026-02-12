use std::collections::HashSet;

use crate::converter::{convert_nbest, convert_nbest_with_history, ConvertedSegment};
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

    // 1. Reading (kana) itself as first candidate
    seen.insert(reading.to_string());
    surfaces.push(reading.to_string());

    // 2. Predictions (ranked by history if available)
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

    // 3. N-best Viterbi conversion
    let nbest = 5usize;
    let paths = if let Some(h) = history {
        convert_nbest_with_history(dict, conn, h, reading, nbest)
    } else {
        convert_nbest(dict, conn, reading, nbest)
    };

    let mut nbest_paths = Vec::new();
    for path in &paths {
        let joined: String = path.iter().map(|s| s.surface.as_str()).collect();
        if !joined.is_empty() && seen.insert(joined.clone()) {
            surfaces.push(joined);
        }
        nbest_paths.push(path.clone());
    }

    // 4. Dictionary lookup
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
    if reading.is_empty() {
        return CandidateResponse {
            surfaces: Vec::new(),
            paths: Vec::new(),
        };
    }

    if punctuation_alternatives(reading).is_some() {
        generate_punctuation_candidates(dict, history, reading, max_results)
    } else {
        generate_normal_candidates(dict, conn, history, reading, max_results)
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
        // First candidate should be the reading itself
        assert_eq!(resp.surfaces[0], "きょう");
        // Should include 今日 and 京
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
}
