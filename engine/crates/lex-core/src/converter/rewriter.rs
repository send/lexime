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
                    self.kanji_variants_exact(path, seg_idx, seg_start, seg_end, &mut new_paths);
                } else {
                    self.kanji_variants_subsplit(path, seg_idx, seg_start, seg_end, &mut new_paths);
                }
            }
        }

        // Phase 2: Reading-scan for single-segment hiragana paths.
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
    /// Top kanji node indices at [start, end), sorted by cost, up to MAX_KANJI_PER_SEGMENT.
    fn top_kanji_at(&self, start: usize, end: usize) -> Vec<usize> {
        let Some(indices) = self.lattice.nodes_by_start.get(start) else {
            return Vec::new();
        };
        let mut kanji: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&idx| {
                self.lattice.end(idx) == end && self.lattice.surface(idx).chars().any(is_kanji)
            })
            .collect();
        kanji.sort_by_key(|&idx| self.lattice.cost(idx));
        kanji.truncate(MAX_KANJI_PER_SEGMENT);
        kanji
    }

    /// Replace a 2-char hiragana segment with kanji alternatives from the lattice.
    fn kanji_variants_exact(
        &self,
        path: &ScoredPath,
        seg_idx: usize,
        seg_start: usize,
        seg_end: usize,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        for idx in self.top_kanji_at(seg_start, seg_end) {
            let mut new_segments = path.segments.clone();
            new_segments[seg_idx] = self.lattice.to_rich_segment(idx);
            new_paths.push(ScoredPath {
                segments: new_segments,
                viterbi_cost: path.viterbi_cost.saturating_add(2000),
            });
        }
    }

    /// For a 3+ char hiragana segment, try splitting at the 2-char boundary
    /// to find a 2-char kanji prefix with a hiragana remainder.
    fn kanji_variants_subsplit(
        &self,
        path: &ScoredPath,
        seg_idx: usize,
        seg_start: usize,
        seg_end: usize,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        let mid = seg_start + 2;
        if mid >= seg_end {
            return;
        }

        let kanji_indices = self.top_kanji_at(seg_start, mid);
        if kanji_indices.is_empty() {
            return;
        }

        // Find a hiragana node for the right part [mid, seg_end)
        let Some(right_indices) = self.lattice.nodes_by_start.get(mid) else {
            return;
        };
        let right_idx = right_indices
            .iter()
            .copied()
            .filter(|&idx| {
                let s = self.lattice.surface(idx);
                self.lattice.end(idx) == seg_end
                    && s == self.lattice.reading(idx)
                    && s.chars().all(is_hiragana)
            })
            .min_by_key(|&idx| self.lattice.cost(idx));
        let Some(right_idx) = right_idx else {
            return;
        };

        for kanji_idx in kanji_indices {
            let mut new_segments = path.segments.clone();
            let right_seg = self.lattice.to_rich_segment(right_idx);
            new_segments[seg_idx] = self.lattice.to_rich_segment(kanji_idx);
            new_segments.insert(seg_idx + 1, right_seg);
            new_paths.push(ScoredPath {
                segments: new_segments,
                viterbi_cost: path.viterbi_cost.saturating_add(2000),
            });
        }
    }

    /// Scan the full reading for 2-char positions that have kanji alternatives
    /// in the lattice, and build single-segment variants with the kanji inlined.
    fn kanji_variants_from_reading(
        &self,
        reading: &str,
        base_cost: i64,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        let byte_offsets: Vec<usize> = reading
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(reading.len()))
            .collect();
        let char_count = byte_offsets.len() - 1;
        if char_count < 3 {
            return;
        }

        for pos in 1..char_count.saturating_sub(2) {
            let end = pos + 2;
            let kanji_indices = self.top_kanji_at(pos, end);
            if kanji_indices.is_empty() {
                continue;
            }

            let prefix = &reading[..byte_offsets[pos]];
            let suffix = &reading[byte_offsets[end]..];

            for idx in kanji_indices {
                let surface = format!("{}{}{}", prefix, self.lattice.surface(idx), suffix);
                new_paths.push(ScoredPath::single(
                    reading.to_string(),
                    surface,
                    base_cost.saturating_add(2000),
                ));
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
