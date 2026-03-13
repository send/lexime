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
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
        let mut new_paths = Vec::new();

        // Phase 1: Segment-based replacement on multi-segment paths.
        // Consider up to 5 eligible source paths (with more than one segment),
        // so that single-segment candidates added by earlier rewriters do not
        // consume this rewriter's processing budget.
        for path in paths.iter().filter(|p| p.segments.len() > 1).take(5) {
            let mut char_pos = 0usize;
            for seg_idx in 0..path.segments.len() {
                let seg = &path.segments[seg_idx];
                let seg_char_len = seg.reading.chars().count();
                let seg_start = char_pos;
                let seg_end = char_pos + seg_char_len;
                char_pos = seg_end;

                // Skip non-hiragana or already-kanji segments
                if seg.surface != seg.reading || !seg.surface.chars().all(is_hiragana) {
                    continue;
                }

                if seg_char_len == 1 {
                    // Single-char segments are almost always function morphemes
                    // (し, た, な, が) where kanji replacements would be incorrect.
                    continue;
                }

                if seg_char_len == 2 {
                    // Exact match: find kanji nodes at the same [start, end) span
                    self.kanji_variants_exact(path, seg_idx, seg_start, seg_end, &mut new_paths);
                } else {
                    // 3+ char hiragana segment (e.g. ほうが): try splitting into
                    // a 2-char kanji prefix + hiragana remainder. This handles
                    // cases where the Viterbi chose a compound segment (cheaper
                    // due to no connection cost) but we want the kanji sub-form
                    // (e.g. ほうが → 方+が).
                    self.kanji_variants_subsplit(path, seg_idx, seg_start, seg_end, &mut new_paths);
                }
            }
        }

        // Phase 2: Reading-scan for single-segment hiragana paths.
        // When HiraganaVariantRewriter produces a single-segment all-hiragana
        // path, the segment-based approach above can't find kanji sub-spans.
        // Scan the reading directly to find 2-char kanji alternatives in the
        // lattice and build 3-segment variants (prefix + kanji + suffix).
        if let Some(base) = paths.iter().find(|p| {
            p.segments.len() == 1
                && p.segments[0].surface == p.segments[0].reading
                && p.segments[0].surface.chars().all(is_hiragana)
        }) {
            self.kanji_variants_from_reading(reading, base.viterbi_cost, &mut new_paths);
        }

        new_paths
    }
}

impl KanjiVariantRewriter<'_> {
    /// Replace a 2-char hiragana segment with kanji alternatives from the lattice.
    fn kanji_variants_exact(
        &self,
        path: &ScoredPath,
        seg_idx: usize,
        seg_start: usize,
        seg_end: usize,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        let node_indices = match self.lattice.nodes_by_start.get(seg_start) {
            Some(indices) => indices,
            None => return,
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

    /// For a 3+ char hiragana segment, try splitting at each internal boundary
    /// to find a 2-char kanji prefix with a hiragana remainder.
    ///
    /// Example: segment "ほうが" [5,8) → split at 7 → kanji "方" [5,7) + "が" [7,8)
    fn kanji_variants_subsplit(
        &self,
        path: &ScoredPath,
        seg_idx: usize,
        seg_start: usize,
        seg_end: usize,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        // Split at the 2-char boundary only (to avoid incorrect boundaries
        // like たほう → 他方).
        let mid = seg_start + 2;
        if mid >= seg_end {
            return;
        }

        // Find kanji nodes for the left part [seg_start, mid)
        let left_indices = match self.lattice.nodes_by_start.get(seg_start) {
            Some(indices) => indices,
            None => return,
        };
        let mut kanji_nodes: Vec<_> = left_indices
            .iter()
            .map(|&idx| &self.lattice.nodes[idx])
            .filter(|node| node.end == mid && node.surface.chars().any(is_kanji))
            .collect();
        kanji_nodes.sort_by_key(|n| n.cost);
        kanji_nodes.truncate(MAX_KANJI_PER_SEGMENT);

        if kanji_nodes.is_empty() {
            return;
        }

        // Find a hiragana node for the right part [mid, seg_end)
        let right_indices = match self.lattice.nodes_by_start.get(mid) {
            Some(indices) => indices,
            None => return,
        };
        // Pick the lowest-cost hiragana node for the remainder
        let right_node = right_indices
            .iter()
            .map(|&idx| &self.lattice.nodes[idx])
            .filter(|node| {
                node.end == seg_end
                    && node.surface == node.reading
                    && node.surface.chars().all(is_hiragana)
            })
            .min_by_key(|n| n.cost);
        let Some(right_node) = right_node else {
            return;
        };

        for kanji_node in kanji_nodes {
            let mut new_segments = path.segments.clone();
            let right_seg = super::viterbi::RichSegment::from(right_node);
            new_segments[seg_idx] = super::viterbi::RichSegment::from(kanji_node);
            new_segments.insert(seg_idx + 1, right_seg);
            new_paths.push(ScoredPath {
                segments: new_segments,
                viterbi_cost: path.viterbi_cost.saturating_add(2000),
            });
        }
    }

    /// Scan the full reading for 2-char positions that have kanji alternatives
    /// in the lattice, and build 3-segment variants (hiragana prefix + kanji + hiragana suffix).
    ///
    /// This handles cases where the only hiragana path is single-segment
    /// (from HiraganaVariantRewriter) and the multi-segment paths all have
    /// kanji/compound segments that don't expose the 2-char hiragana sub-span.
    ///
    /// Example: reading "しておいたほうが" → finds 方 at [5,7) →
    /// builds "しておいた" + "方" + "が"
    fn kanji_variants_from_reading(
        &self,
        reading: &str,
        base_cost: i64,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        let char_count = reading.chars().count();
        if char_count < 3 {
            return;
        }

        // Start at pos=1 (skip pos=0 — no prefix) and stop when end would
        // reach char_count (no suffix). This also ensures prefix/suffix are
        // at least 1-char each.
        for pos in 1..char_count.saturating_sub(2) {
            let end = pos + 2;

            let node_indices = match self.lattice.nodes_by_start.get(pos) {
                Some(indices) => indices,
                None => continue,
            };

            let mut kanji_nodes: Vec<_> = node_indices
                .iter()
                .map(|&idx| &self.lattice.nodes[idx])
                .filter(|node| node.end == end && node.surface.chars().any(is_kanji))
                .collect();
            kanji_nodes.sort_by_key(|n| n.cost);
            kanji_nodes.truncate(MAX_KANJI_PER_SEGMENT);

            if kanji_nodes.is_empty() {
                continue;
            }

            // Build prefix reading [0, pos) and suffix reading [end, char_count)
            let prefix_reading: String = reading.chars().take(pos).collect();
            let suffix_reading: String = reading.chars().skip(end).collect();

            for node in kanji_nodes {
                let segments = vec![
                    super::viterbi::RichSegment {
                        reading: prefix_reading.clone(),
                        surface: prefix_reading.clone(),
                        left_id: 0,
                        right_id: 0,
                        word_cost: 0,
                    },
                    super::viterbi::RichSegment::from(node),
                    super::viterbi::RichSegment {
                        reading: suffix_reading.clone(),
                        surface: suffix_reading.clone(),
                        left_id: 0,
                        right_id: 0,
                        word_cost: 0,
                    },
                ];
                new_paths.push(ScoredPath {
                    segments,
                    viterbi_cost: base_cost.saturating_add(2000),
                });
            }
        }
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
