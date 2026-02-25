use crate::numeric;
use crate::unicode::{hiragana_to_katakana, is_katakana};

use super::viterbi::ScoredPath;

/// A rewriter that can add or modify candidates in the N-best list.
pub(crate) trait Rewriter {
    fn rewrite(&self, paths: &mut Vec<ScoredPath>, reading: &str);
}

/// Worst (highest) Viterbi cost among paths, or 0 if empty.
fn worst_cost(paths: &[ScoredPath]) -> i64 {
    paths.iter().map(|p| p.viterbi_cost).max().unwrap_or(0)
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
        let wc = worst_cost(paths);

        paths.push(ScoredPath::single(
            reading.to_string(),
            katakana,
            wc.saturating_add(10000),
        ));
    }
}

/// Adds a hiragana variant of the best Viterbi path by replacing kanji segments
/// with their reading while keeping katakana and hiragana segments as-is.
///
/// Example: `リダイレクト|去れ|ます|化` → `リダイレクトされますか`
pub(crate) struct HiraganaVariantRewriter;

impl Rewriter for HiraganaVariantRewriter {
    fn rewrite(&self, paths: &mut Vec<ScoredPath>, _reading: &str) {
        let Some(best) = paths.first() else {
            return;
        };

        let mut any_replaced = false;
        let mut combined_reading = String::new();
        let mut combined_surface = String::new();

        for seg in &best.segments {
            if seg.surface.chars().all(is_katakana) {
                // Katakana → keep as-is
                combined_reading.push_str(&seg.reading);
                combined_surface.push_str(&seg.surface);
            } else if seg.surface == seg.reading {
                // Already hiragana → keep
                combined_reading.push_str(&seg.reading);
                combined_surface.push_str(&seg.surface);
            } else {
                // Kanji → replace with reading
                combined_reading.push_str(&seg.reading);
                combined_surface.push_str(&seg.reading);
                any_replaced = true;
            }
        }

        if !any_replaced {
            return;
        }

        if paths.iter().any(|p| p.surface_key() == combined_surface) {
            return;
        }

        let wc = worst_cost(paths);

        paths.push(ScoredPath::single(
            combined_reading,
            combined_surface,
            wc.saturating_add(5000),
        ));
    }
}

/// For each top-N Viterbi path, generate variants where individual kanji
/// segments are replaced with their hiragana readings.
///
/// Example: `下|方|が|良い` → `した|方|が|良い`
pub(crate) struct PartialHiraganaRewriter;

impl Rewriter for PartialHiraganaRewriter {
    fn rewrite(&self, paths: &mut Vec<ScoredPath>, _reading: &str) {
        let source_count = paths.len().min(5);
        let mut new_paths = Vec::new();

        for i in 0..source_count {
            let path = &paths[i];
            // Single-segment paths are handled elsewhere
            if path.segments.len() <= 1 {
                continue;
            }

            for seg_idx in 0..path.segments.len() {
                let seg = &path.segments[seg_idx];
                // Skip if already hiragana or katakana
                if seg.surface == seg.reading || seg.surface.chars().all(is_katakana) {
                    continue;
                }

                let mut new_segments = path.segments.clone();
                new_segments[seg_idx].surface = new_segments[seg_idx].reading.clone();

                let surface: String = new_segments.iter().map(|s| s.surface.as_str()).collect();

                // Dedup against existing paths and new paths
                if paths.iter().any(|p| p.surface_key() == surface)
                    || new_paths
                        .iter()
                        .any(|p: &ScoredPath| p.surface_key() == surface)
                {
                    continue;
                }

                new_paths.push(ScoredPath {
                    segments: new_segments,
                    viterbi_cost: path.viterbi_cost.saturating_add(2000),
                });
            }
        }

        paths.extend(new_paths);
    }
}

/// Adds numeric candidates (half-width and full-width) when the reading is a
/// Japanese number expression.
pub(crate) struct NumericRewriter;

impl Rewriter for NumericRewriter {
    fn rewrite(&self, paths: &mut Vec<ScoredPath>, reading: &str) {
        let Some(n) = numeric::parse_japanese_number(reading) else {
            return;
        };
        let best_cost = paths.iter().map(|p| p.viterbi_cost).min().unwrap_or(0);
        let base_cost = worst_cost(paths).saturating_add(5000);

        // Kanji candidate
        let kanji = numeric::to_kanji(n);
        let is_compound = kanji.chars().count() > 1;
        if !paths.iter().any(|p| p.surface_key() == kanji) {
            let kanji_cost = if is_compound { best_cost } else { base_cost };
            let kanji_path = ScoredPath::single(reading.to_string(), kanji, kanji_cost);
            if is_compound {
                // Compound kanji (二十三, 三百) → insert at top
                paths.insert(0, kanji_path);
            } else {
                // Single-char kanji (十, 百) → low priority, let dictionary entries win
                paths.push(kanji_path);
            }
        }

        // Half-width Arabic digits
        let halfwidth = numeric::to_halfwidth(n);
        if !paths.iter().any(|p| p.surface_key() == halfwidth) {
            paths.push(ScoredPath::single(
                reading.to_string(),
                halfwidth,
                base_cost,
            ));
        }

        // Full-width Arabic digits
        let fullwidth = numeric::to_fullwidth(n);
        if !paths.iter().any(|p| p.surface_key() == fullwidth) {
            paths.push(ScoredPath::single(
                reading.to_string(),
                fullwidth,
                base_cost.saturating_add(1),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::viterbi::RichSegment;
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

    #[test]
    fn test_numeric_rewriter_adds_candidates() {
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

        rw.rewrite(&mut paths, "にじゅうさん");

        // Compound kanji inserted at position 0 with best_cost
        assert_eq!(paths.len(), 4);
        assert_eq!(paths[0].surface_key(), "二十三");
        assert_eq!(paths[0].viterbi_cost, 3000);
        assert_eq!(paths[1].surface_key(), "に十三");
        assert_eq!(paths[2].surface_key(), "23");
        assert_eq!(paths[2].viterbi_cost, 3000 + 5000);
        assert_eq!(paths[3].surface_key(), "２３");
        assert_eq!(paths[3].viterbi_cost, 3000 + 5001);
    }

    #[test]
    fn test_numeric_rewriter_kanji_duplicate_skip() {
        // When kanji candidate already exists in Viterbi paths, skip it
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

        rw.rewrite(&mut paths, "にじゅうさん");

        // Kanji already exists, only halfwidth + fullwidth added
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0].surface_key(), "二十三");
        assert_eq!(paths[1].surface_key(), "23");
        assert_eq!(paths[2].surface_key(), "２３");
    }

    #[test]
    fn test_numeric_rewriter_single_char_kanji_low_priority() {
        // Single-char kanji like 十 should be low priority
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

        rw.rewrite(&mut paths, "じゅう");

        // 十 is single-char → pushed at end with base_cost
        assert_eq!(paths[0].surface_key(), "中");
        assert_eq!(paths[1].surface_key(), "十");
        assert_eq!(paths[1].viterbi_cost, 3000 + 5000);
    }

    #[test]
    fn test_numeric_rewriter_skips_non_numeric() {
        let rw = NumericRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "今日".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        rw.rewrite(&mut paths, "きょう");

        assert_eq!(
            paths.len(),
            1,
            "should not add numeric candidates for non-numeric input"
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

        rw.rewrite(&mut paths, "いち");

        // Half-width "1" already exists; kanji "一" (single-char) + full-width "１" added
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[1].surface_key(), "一");
        assert_eq!(paths[1].viterbi_cost, 1000 + 5000); // single-char → low priority
        assert_eq!(paths[2].surface_key(), "１");
    }

    #[test]
    fn test_hiragana_variant_replaces_kanji() {
        let rw = HiraganaVariantRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "りだいれくとされますか");

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1].surface_key(), "リダイレクトされますか");
        assert_eq!(paths[1].viterbi_cost, 3000 + 5000);
    }

    #[test]
    fn test_hiragana_variant_skips_all_hiragana() {
        let rw = HiraganaVariantRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "されます");

        assert_eq!(
            paths.len(),
            1,
            "should not add variant when all segments are already hiragana"
        );
    }

    #[test]
    fn test_hiragana_variant_skips_duplicate() {
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

        rw.rewrite(&mut paths, "され");

        assert_eq!(paths.len(), 2, "should not add duplicate hiragana variant");
    }

    #[test]
    fn test_hiragana_variant_keeps_katakana() {
        let rw = HiraganaVariantRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "てすとちゅう");

        assert_eq!(paths.len(), 2);
        // Katakana "テスト" kept, kanji "中" replaced with "ちゅう"
        assert_eq!(paths[1].surface_key(), "テストちゅう");
    }

    // ── PartialHiraganaRewriter tests ──────────────────────────────

    #[test]
    fn test_partial_hiragana_basic() {
        let rw = PartialHiraganaRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "したほう");

        // Should produce 2 variants: した|方 and 下|ほう
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p.surface_key() == "した方"));
        assert!(paths.iter().any(|p| p.surface_key() == "下ほう"));
        // Cost should be original + 2000
        assert_eq!(paths[1].viterbi_cost, 5000);
    }

    #[test]
    fn test_partial_hiragana_multiple_kanji() {
        let rw = PartialHiraganaRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "したほうが");

        // Two kanji segments → 2 variants: した|法|が and 舌|ほう|が
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p.surface_key() == "した法が"));
        assert!(paths.iter().any(|p| p.surface_key() == "舌ほうが"));
    }

    #[test]
    fn test_partial_hiragana_dedup() {
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

        rw.rewrite(&mut paths, "したほう");

        // "した方" already exists in paths, should not be duplicated
        let count = paths.iter().filter(|p| p.surface_key() == "した方").count();
        assert_eq!(count, 1, "should not add duplicate した方");
    }

    #[test]
    fn test_partial_hiragana_all_hiragana_no_variants() {
        let rw = PartialHiraganaRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "したほう");

        assert_eq!(
            paths.len(),
            1,
            "all-hiragana path should produce no variants"
        );
    }

    #[test]
    fn test_partial_hiragana_keeps_katakana() {
        let rw = PartialHiraganaRewriter;
        let mut paths = vec![ScoredPath {
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

        rw.rewrite(&mut paths, "てすとちゅう");

        // Only 中→ちゅう variant, katakana テスト should NOT be replaced
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1].surface_key(), "テストちゅう");
    }

    #[test]
    fn test_partial_hiragana_single_segment_skip() {
        let rw = PartialHiraganaRewriter;
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "した".into(),
                surface: "下".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        }];

        rw.rewrite(&mut paths, "した");

        assert_eq!(paths.len(), 1, "single-segment path should be skipped");
    }
}
