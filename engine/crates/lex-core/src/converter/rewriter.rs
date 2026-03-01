use std::collections::HashSet;

use crate::numeric;
use crate::unicode::{hiragana_to_katakana, is_hiragana, is_kanji, is_katakana};

use super::lattice::Lattice;
use super::viterbi::ScoredPath;

/// A rewriter that generates new candidates from the N-best list.
///
/// Implementations return new candidates without mutating the input.
/// Deduplication and cost-ordered insertion are handled by `run_rewriters`.
pub(crate) trait Rewriter {
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath>;
}

/// Worst (highest) Viterbi cost among paths, or 0 if empty.
fn worst_cost(paths: &[ScoredPath]) -> i64 {
    paths.iter().map(|p| p.viterbi_cost).max().unwrap_or(0)
}

/// Run all rewriters in sequence, deduplicating and inserting in cost order.
pub(crate) fn run_rewriters(
    rewriters: &[&dyn Rewriter],
    paths: &mut Vec<ScoredPath>,
    reading: &str,
) {
    let mut seen: HashSet<String> = paths.iter().map(|p| p.surface_key()).collect();
    for rw in rewriters {
        let candidates = rw.generate(paths, reading);
        for candidate in candidates {
            if seen.insert(candidate.surface_key()) {
                let pos = paths.partition_point(|p| p.viterbi_cost < candidate.viterbi_cost);
                paths.insert(pos, candidate);
            }
        }
    }
}

/// Adds a katakana candidate to the N-best list.
///
/// The candidate is always appended with a cost higher than the worst
/// existing path, so it appears as a low-priority fallback.
pub(crate) struct KatakanaRewriter;

impl Rewriter for KatakanaRewriter {
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
        let katakana = hiragana_to_katakana(reading);
        let wc = worst_cost(paths);
        vec![ScoredPath::single(
            reading.to_string(),
            katakana,
            wc.saturating_add(10000),
        )]
    }
}

/// Adds a hiragana variant of the best Viterbi path by replacing kanji segments
/// with their reading while keeping katakana and hiragana segments as-is.
///
/// Example: `リダイレクト|去れ|ます|化` → `リダイレクトされますか`
pub(crate) struct HiraganaVariantRewriter;

impl Rewriter for HiraganaVariantRewriter {
    fn generate(&self, paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        let Some(best) = paths.first() else {
            return Vec::new();
        };

        let mut any_replaced = false;
        let mut combined_reading = String::new();
        let mut combined_surface = String::new();

        for seg in &best.segments {
            combined_reading.push_str(&seg.reading);
            if seg.surface.chars().all(is_katakana) || seg.surface == seg.reading {
                // Katakana or already hiragana → keep as-is
                combined_surface.push_str(&seg.surface);
            } else {
                // Kanji → replace with reading
                combined_surface.push_str(&seg.reading);
                any_replaced = true;
            }
        }

        if !any_replaced {
            return Vec::new();
        }

        let wc = worst_cost(paths);
        vec![ScoredPath::single(
            combined_reading,
            combined_surface,
            wc.saturating_add(5000),
        )]
    }
}

/// For each top-N Viterbi path, generate variants where individual kanji
/// segments are replaced with their hiragana readings.
///
/// Example: `下|方|が|良い` → `した|方|が|良い`
pub(crate) struct PartialHiraganaRewriter;

impl Rewriter for PartialHiraganaRewriter {
    fn generate(&self, paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        let source_count = paths.len().min(5);
        let mut new_paths = Vec::new();

        for path in paths.iter().take(source_count) {
            if path.segments.len() <= 1 {
                continue;
            }

            for seg_idx in 0..path.segments.len() {
                let seg = &path.segments[seg_idx];
                if seg.surface == seg.reading || seg.surface.chars().all(is_katakana) {
                    continue;
                }

                let mut new_segments = path.segments.clone();
                new_segments[seg_idx].surface = new_segments[seg_idx].reading.clone();

                new_paths.push(ScoredPath {
                    segments: new_segments,
                    viterbi_cost: path.viterbi_cost.saturating_add(2000),
                });
            }
        }

        new_paths
    }
}

/// For each top-N Viterbi path, generate variants where individual hiragana
/// segments are replaced with kanji alternatives from the lattice.
///
/// This is the reverse of `PartialHiraganaRewriter`: instead of softening
/// kanji → hiragana, it surfaces kanji alternatives that the Viterbi
/// may have skipped due to higher word cost.
///
/// Example: `あった|ほう|が` → `あった|方|が`
pub(crate) struct KanjiVariantRewriter<'a> {
    pub lattice: &'a Lattice,
}

/// Maximum number of kanji alternatives per hiragana segment.
const MAX_KANJI_PER_SEGMENT: usize = 3;

impl Rewriter for KanjiVariantRewriter<'_> {
    fn generate(&self, paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        let source_count = paths.len().min(5);
        let mut new_paths = Vec::new();

        for path in paths.iter().take(source_count) {
            if path.segments.len() <= 1 {
                continue;
            }

            let mut char_pos = 0usize;
            for seg_idx in 0..path.segments.len() {
                let seg = &path.segments[seg_idx];
                let seg_char_len = seg.reading.chars().count();
                let seg_start = char_pos;
                let seg_end = char_pos + seg_char_len;
                char_pos = seg_end;

                // Only process 2-char hiragana segments. Single-char segments
                // are skipped because they are almost always function morphemes
                // (し, た, な, が) where kanji replacements would be incorrect.
                // Segments of 3+ chars are skipped because they often come from
                // resegmentation with incorrect morpheme boundaries
                // (e.g. たほう → 他方).
                if seg_char_len != 2
                    || seg.surface != seg.reading
                    || !seg.surface.chars().all(is_hiragana)
                {
                    continue;
                }

                // Find kanji nodes at the same [start, end) span in the lattice
                let node_indices = match self.lattice.nodes_by_start.get(seg_start) {
                    Some(indices) => indices,
                    None => continue,
                };

                let mut kanji_nodes: Vec<_> = node_indices
                    .iter()
                    .map(|&idx| &self.lattice.nodes[idx])
                    .filter(|node| node.end == seg_end && node.surface.chars().any(is_kanji))
                    .collect();
                kanji_nodes.sort_by_key(|n| n.cost);
                kanji_nodes.truncate(MAX_KANJI_PER_SEGMENT);

                for node in kanji_nodes {
                    let mut new_segments = path.segments.clone();
                    new_segments[seg_idx] = super::viterbi::RichSegment::from(node);
                    new_paths.push(ScoredPath {
                        segments: new_segments,
                        viterbi_cost: path.viterbi_cost.saturating_add(2000),
                    });
                }
            }
        }

        new_paths
    }
}

/// Adds numeric candidates (half-width and full-width) when the reading is a
/// Japanese number expression.
pub(crate) struct NumericRewriter;

impl Rewriter for NumericRewriter {
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
        let Some(n) = numeric::parse_japanese_number(reading) else {
            return Vec::new();
        };
        let best_cost = paths.iter().map(|p| p.viterbi_cost).min().unwrap_or(0);
        let base_cost = worst_cost(paths).saturating_add(5000);

        let mut candidates = Vec::new();

        // Kanji candidate
        let kanji = numeric::to_kanji(n);
        let is_compound = kanji.chars().count() > 1;
        let kanji_cost = if is_compound { best_cost } else { base_cost };
        candidates.push(ScoredPath::single(reading.to_string(), kanji, kanji_cost));

        // Half-width Arabic digits
        let halfwidth = numeric::to_halfwidth(n);
        candidates.push(ScoredPath::single(
            reading.to_string(),
            halfwidth,
            base_cost,
        ));

        // Full-width Arabic digits
        let fullwidth = numeric::to_fullwidth(n);
        candidates.push(ScoredPath::single(
            reading.to_string(),
            fullwidth,
            base_cost.saturating_add(1),
        ));

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::super::viterbi::RichSegment;
    use super::*;

    #[test]
    fn test_katakana_rewriter_generates_candidate() {
        let rw = KatakanaRewriter;
        let paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "今日".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        }];

        let result = rw.generate(&paths, "きょう");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface_key(), "キョウ");
        assert_eq!(result[0].viterbi_cost, 3000 + 10000);
    }

    #[test]
    fn test_katakana_dedup_via_run_rewriters() {
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

        run_rewriters(&[&rw], &mut paths, "きょう");

        assert_eq!(
            paths.len(),
            1,
            "should not add duplicate katakana candidate"
        );
    }

    #[test]
    fn test_katakana_rewriter_empty_paths() {
        let rw = KatakanaRewriter;
        let paths: Vec<ScoredPath> = Vec::new();

        let result = rw.generate(&paths, "てすと");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface_key(), "テスト");
        assert_eq!(result[0].viterbi_cost, 10000);
    }

    #[test]
    fn test_run_rewriters_applies_all() {
        let rw = KatakanaRewriter;
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

        run_rewriters(&[&rw], &mut paths, "あ");

        assert_eq!(paths.len(), 2);
        // Katakana has higher cost, so inserted after 亜
        assert_eq!(paths[0].surface_key(), "亜");
        assert_eq!(paths[1].surface_key(), "ア");
    }

    #[test]
    fn test_run_rewriters_dedup_across_rewriters() {
        // HiraganaVariant and PartialHiragana could produce the same surface;
        // run_rewriters should keep only the first one.
        let hiragana_rw = HiraganaVariantRewriter;
        let partial_rw = PartialHiraganaRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "され".into(),
                    surface: "去れ".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ます".into(),
                    surface: "ます".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 1000,
        }];

        run_rewriters(&[&hiragana_rw, &partial_rw], &mut paths, "されます");

        // Both would generate "されます", but only one copy should exist
        let count = paths
            .iter()
            .filter(|p| p.surface_key() == "されます")
            .count();
        assert_eq!(count, 1, "dedup should prevent duplicate across rewriters");
    }

    #[test]
    fn test_run_rewriters_cost_ordered_insertion() {
        // Compound kanji (best_cost) should be inserted at position 0
        let rw = NumericRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "にじゅうさん".into(),
                surface: "に十三".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        }];

        run_rewriters(&[&rw], &mut paths, "にじゅうさん");

        assert_eq!(paths[0].surface_key(), "二十三");
        assert_eq!(paths[0].viterbi_cost, 3000); // best_cost = 3000
        assert_eq!(paths[1].surface_key(), "に十三");
    }

    #[test]
    fn test_numeric_rewriter_generates_candidates() {
        let rw = NumericRewriter;
        let paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "にじゅうさん".into(),
                surface: "に十三".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        }];

        let result = rw.generate(&paths, "にじゅうさん");

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].surface_key(), "二十三");
        assert_eq!(result[0].viterbi_cost, 3000); // compound → best_cost
        assert_eq!(result[1].surface_key(), "23");
        assert_eq!(result[1].viterbi_cost, 3000 + 5000);
        assert_eq!(result[2].surface_key(), "２３");
        assert_eq!(result[2].viterbi_cost, 3000 + 5001);
    }

    #[test]
    fn test_numeric_rewriter_kanji_duplicate_skip() {
        let rw = NumericRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "にじゅうさん".into(),
                surface: "二十三".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        }];

        run_rewriters(&[&rw], &mut paths, "にじゅうさん");

        // Kanji already exists, only halfwidth + fullwidth added
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0].surface_key(), "二十三");
        assert_eq!(paths[1].surface_key(), "23");
        assert_eq!(paths[2].surface_key(), "２３");
    }

    #[test]
    fn test_numeric_rewriter_single_char_kanji_low_priority() {
        let rw = NumericRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "じゅう".into(),
                surface: "中".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        }];

        run_rewriters(&[&rw], &mut paths, "じゅう");

        // 十 is single-char → base_cost (not best_cost), all after 中
        assert_eq!(paths[0].surface_key(), "中");
        let kanji = paths.iter().find(|p| p.surface_key() == "十").unwrap();
        assert_eq!(kanji.viterbi_cost, 3000 + 5000);
    }

    #[test]
    fn test_numeric_rewriter_skips_non_numeric() {
        let rw = NumericRewriter;
        let paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "今日".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "きょう");

        assert!(
            result.is_empty(),
            "should not generate numeric candidates for non-numeric input"
        );
    }

    #[test]
    fn test_numeric_rewriter_skips_duplicate() {
        let rw = NumericRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "いち".into(),
                surface: "1".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        run_rewriters(&[&rw], &mut paths, "いち");

        // Half-width "1" already exists; kanji "一" (single-char) + full-width "１" added
        assert_eq!(paths.len(), 3);
        // All have high cost, so they come after "1"
        assert_eq!(paths[0].surface_key(), "1");
        assert!(paths.iter().any(|p| p.surface_key() == "一"));
        assert!(paths.iter().any(|p| p.surface_key() == "１"));
    }

    #[test]
    fn test_hiragana_variant_replaces_kanji() {
        let rw = HiraganaVariantRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "りだいれくと".into(),
                    surface: "リダイレクト".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "され".into(),
                    surface: "去れ".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ます".into(),
                    surface: "ます".into(),
                    left_id: 30,
                    right_id: 30,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "か".into(),
                    surface: "化".into(),
                    left_id: 40,
                    right_id: 40,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 3000,
        }];

        let result = rw.generate(&paths, "りだいれくとされますか");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface_key(), "リダイレクトされますか");
        assert_eq!(result[0].viterbi_cost, 3000 + 5000);
    }

    #[test]
    fn test_hiragana_variant_skips_all_hiragana() {
        let rw = HiraganaVariantRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "され".into(),
                    surface: "され".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ます".into(),
                    surface: "ます".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "されます");

        assert!(
            result.is_empty(),
            "should not add variant when all segments are already hiragana"
        );
    }

    #[test]
    fn test_hiragana_variant_dedup_via_run_rewriters() {
        let rw = HiraganaVariantRewriter;
        let mut paths = vec![
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "され".into(),
                    surface: "去れ".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                }],
                viterbi_cost: 3000,
            },
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "され".into(),
                    surface: "され".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                }],
                viterbi_cost: 4000,
            },
        ];

        run_rewriters(&[&rw], &mut paths, "され");

        assert_eq!(paths.len(), 2, "should not add duplicate hiragana variant");
    }

    #[test]
    fn test_hiragana_variant_keeps_katakana() {
        let rw = HiraganaVariantRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "てすと".into(),
                    surface: "テスト".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ちゅう".into(),
                    surface: "中".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 2000,
        }];

        let result = rw.generate(&paths, "てすとちゅう");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface_key(), "テストちゅう");
    }

    // ── PartialHiraganaRewriter tests ──────────────────────────────

    #[test]
    fn test_partial_hiragana_basic() {
        let rw = PartialHiraganaRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "した".into(),
                    surface: "下".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "方".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 3000,
        }];

        let result = rw.generate(&paths, "したほう");

        // Should produce 2 variants: した|方 and 下|ほう
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.surface_key() == "した方"));
        assert!(result.iter().any(|p| p.surface_key() == "下ほう"));
        assert!(result.iter().all(|p| p.viterbi_cost == 5000));
    }

    #[test]
    fn test_partial_hiragana_multiple_kanji() {
        let rw = PartialHiraganaRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "した".into(),
                    surface: "舌".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "法".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "が".into(),
                    surface: "が".into(),
                    left_id: 30,
                    right_id: 30,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "したほうが");

        // Two kanji segments → 2 variants: した|法|が and 舌|ほう|が
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.surface_key() == "した法が"));
        assert!(result.iter().any(|p| p.surface_key() == "舌ほうが"));
    }

    #[test]
    fn test_partial_hiragana_dedup_via_run_rewriters() {
        let rw = PartialHiraganaRewriter;
        let mut paths = vec![
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "した".into(),
                        surface: "下".into(),
                        left_id: 10,
                        right_id: 10,
                        word_cost: 0,
                    },
                    RichSegment {
                        reading: "ほう".into(),
                        surface: "方".into(),
                        left_id: 20,
                        right_id: 20,
                        word_cost: 0,
                    },
                ],
                viterbi_cost: 3000,
            },
            // This path already has the surface "した方"
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "した".into(),
                        surface: "した".into(),
                        left_id: 0,
                        right_id: 0,
                        word_cost: 0,
                    },
                    RichSegment {
                        reading: "ほう".into(),
                        surface: "方".into(),
                        left_id: 20,
                        right_id: 20,
                        word_cost: 0,
                    },
                ],
                viterbi_cost: 5000,
            },
        ];

        run_rewriters(&[&rw], &mut paths, "したほう");

        // "した方" already exists in paths, should not be duplicated
        let count = paths.iter().filter(|p| p.surface_key() == "した方").count();
        assert_eq!(count, 1, "should not add duplicate した方");
    }

    #[test]
    fn test_partial_hiragana_all_hiragana_no_variants() {
        let rw = PartialHiraganaRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "した".into(),
                    surface: "した".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "ほう".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "したほう");

        assert!(
            result.is_empty(),
            "all-hiragana path should produce no variants"
        );
    }

    #[test]
    fn test_partial_hiragana_keeps_katakana() {
        let rw = PartialHiraganaRewriter;
        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "てすと".into(),
                    surface: "テスト".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ちゅう".into(),
                    surface: "中".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 2000,
        }];

        let result = rw.generate(&paths, "てすとちゅう");

        // Only 中→ちゅう variant, katakana テスト should NOT be replaced
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface_key(), "テストちゅう");
    }

    #[test]
    fn test_partial_hiragana_single_segment_skip() {
        let rw = PartialHiraganaRewriter;
        let paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "した".into(),
                surface: "下".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "した");

        assert!(result.is_empty(), "single-segment path should be skipped");
    }

    // ── KanjiVariantRewriter tests ────────────────────────────────────

    fn make_lattice(input: &str, nodes: Vec<super::super::lattice::LatticeNode>) -> Lattice {
        let char_count = input.chars().count();
        let mut nodes_by_start: Vec<Vec<usize>> = vec![Vec::new(); char_count];
        let mut nodes_by_end: Vec<Vec<usize>> = vec![Vec::new(); char_count + 1];
        for (i, node) in nodes.iter().enumerate() {
            nodes_by_start[node.start].push(i);
            nodes_by_end[node.end].push(i);
        }
        Lattice {
            input: input.to_string(),
            nodes,
            nodes_by_start,
            nodes_by_end,
            char_count,
        }
    }

    fn lattice_node(
        start: usize,
        end: usize,
        reading: &str,
        surface: &str,
        cost: i16,
    ) -> super::super::lattice::LatticeNode {
        super::super::lattice::LatticeNode {
            start,
            end,
            reading: reading.into(),
            surface: surface.into(),
            cost,
            left_id: 0,
            right_id: 0,
        }
    }

    #[test]
    fn test_kanji_variant_replaces_2char_hiragana() {
        // Lattice has ほう → 方 (cost=733) at position [3,5)
        let lattice = make_lattice(
            "あったほうが",
            vec![
                lattice_node(3, 5, "ほう", "ほう", 0),
                lattice_node(3, 5, "ほう", "方", 733),
                lattice_node(3, 5, "ほう", "法", 2181),
            ],
        );
        let rw = KanjiVariantRewriter { lattice: &lattice };

        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "あっ".into(),
                    surface: "あっ".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "た".into(),
                    surface: "た".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "ほう".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "が".into(),
                    surface: "が".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 20000,
        }];

        let result = rw.generate(&paths, "あったほうが");

        // Should produce variants for 方 and 法 (top 3, but only 2 kanji available)
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.surface_key() == "あった方が"));
        assert!(result.iter().any(|p| p.surface_key() == "あった法が"));
        // All variants should have +2000 penalty
        assert!(result.iter().all(|p| p.viterbi_cost == 22000));
    }

    #[test]
    fn test_kanji_variant_skips_single_char() {
        // Single-char hiragana (し) should NOT be replaced
        let lattice = make_lattice("した", vec![lattice_node(0, 1, "し", "死", 500)]);
        let rw = KanjiVariantRewriter { lattice: &lattice };

        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "し".into(),
                    surface: "し".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "た".into(),
                    surface: "た".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "した");

        assert!(result.is_empty(), "single-char hiragana should be skipped");
    }

    #[test]
    fn test_kanji_variant_skips_single_segment() {
        let lattice = make_lattice("ほう", vec![lattice_node(0, 2, "ほう", "方", 733)]);
        let rw = KanjiVariantRewriter { lattice: &lattice };

        let paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "ほう".into(),
                surface: "ほう".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        let result = rw.generate(&paths, "ほう");

        assert!(result.is_empty(), "single-segment path should be skipped");
    }

    #[test]
    fn test_kanji_variant_skips_kanji_segments() {
        // Segments already containing kanji should not be processed
        let lattice = make_lattice("したほう", vec![lattice_node(2, 4, "ほう", "方", 733)]);
        let rw = KanjiVariantRewriter { lattice: &lattice };

        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "した".into(),
                    surface: "下".into(), // kanji — should skip
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "方".into(), // kanji — should skip
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 3000,
        }];

        let result = rw.generate(&paths, "したほう");

        assert!(
            result.is_empty(),
            "kanji segments should not produce variants"
        );
    }

    #[test]
    fn test_kanji_variant_skips_3char_segments() {
        // 3-char hiragana segments should not be replaced
        let lattice = make_lattice("たほうが", vec![lattice_node(0, 3, "たほう", "他方", 5290)]);
        let rw = KanjiVariantRewriter { lattice: &lattice };

        let paths = vec![ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "たほう".into(),
                    surface: "たほう".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "が".into(),
                    surface: "が".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 5000,
        }];

        let result = rw.generate(&paths, "たほうが");

        assert!(
            result.is_empty(),
            "3-char hiragana segment should be skipped"
        );
    }
}
