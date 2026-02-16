use crate::unicode::hiragana_to_katakana;

use super::viterbi::{RichSegment, ScoredPath};

/// A rewriter that can add or modify candidates in the N-best list.
pub(crate) trait Rewriter {
    fn rewrite(&self, paths: &mut Vec<ScoredPath>, reading: &str);
}

/// Run all rewriters in sequence on the N-best path list.
pub(crate) fn run_rewriters(
    rewriters: &[&dyn Rewriter],
    paths: &mut Vec<ScoredPath>,
    reading: &str,
) {
    for rw in rewriters {
        rw.rewrite(paths, reading);
    }
}

/// Adds a katakana candidate to the N-best list.
///
/// The candidate is always appended with a cost higher than the worst
/// existing path, so it appears as a low-priority fallback.
pub(crate) struct KatakanaRewriter;

impl Rewriter for KatakanaRewriter {
    fn rewrite(&self, paths: &mut Vec<ScoredPath>, reading: &str) {
        let katakana = hiragana_to_katakana(reading);

        // Skip if katakana candidate already exists in paths
        if paths.iter().any(|p| p.surface_key() == katakana) {
            return;
        }

        // Cost: worst path + 10000 (always lower priority than Viterbi paths)
        let worst_cost = paths.iter().map(|p| p.viterbi_cost).max().unwrap_or(0);

        paths.push(ScoredPath {
            segments: vec![RichSegment {
                reading: reading.to_string(),
                surface: katakana,
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: worst_cost.saturating_add(10000),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_katakana_rewriter_adds_candidate() {
        let rw = KatakanaRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "今日".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        }];

        rw.rewrite(&mut paths, "きょう");

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1].surface_key(), "キョウ");
        assert_eq!(paths[1].viterbi_cost, 3000 + 10000);
    }

    #[test]
    fn test_katakana_rewriter_skips_duplicate() {
        let rw = KatakanaRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "キョウ".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 5000,
        }];

        rw.rewrite(&mut paths, "きょう");

        assert_eq!(
            paths.len(),
            1,
            "should not add duplicate katakana candidate"
        );
    }

    #[test]
    fn test_katakana_rewriter_empty_paths() {
        let rw = KatakanaRewriter;
        let mut paths: Vec<ScoredPath> = Vec::new();

        rw.rewrite(&mut paths, "てすと");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].surface_key(), "テスト");
        assert_eq!(paths[0].viterbi_cost, 10000);
    }

    #[test]
    fn test_run_rewriters_applies_all() {
        let rw = KatakanaRewriter;
        let rewriters: Vec<&dyn Rewriter> = vec![&rw];
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "あ".into(),
                surface: "亜".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        run_rewriters(&rewriters, &mut paths, "あ");

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1].surface_key(), "ア");
    }
}
