use std::collections::HashSet;

use crate::dict::connection::ConnectionMatrix;
use crate::numeric;
use crate::unicode::{hiragana_to_katakana, is_hiragana, is_kanji, is_katakana};

use super::lattice::Lattice;
use super::viterbi::ScoredPath;

/// Position of a segment within a path: its index and character range.
#[derive(Clone, Copy)]
struct SegmentPos {
    idx: usize,
    start: usize,
    end: usize,
}

impl SegmentPos {
    fn char_range(self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

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

                let spos = SegmentPos {
                    idx: seg_idx,
                    start: seg_start,
                    end: seg_end,
                };
                if seg_char_len == 2 {
                    self.kanji_variants_exact(path, spos, &mut new_paths);
                } else {
                    self.kanji_variants_subsplit(path, spos, &mut new_paths);
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
    /// Top kanji node indices in span `pos` (`[start, end)`), sorted by cost, up to MAX_KANJI_PER_SEGMENT.
    fn top_kanji_at(&self, pos: std::ops::Range<usize>) -> Vec<usize> {
        let Some(indices) = self.lattice.nodes_by_start.get(pos.start) else {
            return Vec::new();
        };
        let mut kanji: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&idx| {
                self.lattice.end(idx) == pos.end && self.lattice.surface(idx).chars().any(is_kanji)
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
        seg: SegmentPos,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        for idx in self.top_kanji_at(seg.char_range()) {
            let mut new_segments = path.segments.clone();
            new_segments[seg.idx] = self.lattice.to_rich_segment(idx);
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
        seg: SegmentPos,
        new_paths: &mut Vec<ScoredPath>,
    ) {
        let mid = seg.start + 2;
        if mid >= seg.end {
            return;
        }

        let kanji_indices = self.top_kanji_at(seg.start..mid);
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
                self.lattice.end(idx) == seg.end
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
            new_segments[seg.idx] = self.lattice.to_rich_segment(kanji_idx);
            new_segments.insert(seg.idx + 1, right_seg);
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
            let kanji_indices = self.top_kanji_at(pos..end);
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

/// Adds numeric candidates when the reading is a Japanese number expression.
///
/// Two recognition modes:
/// 1. Pure number — entire reading parses as a number (e.g. `さんぜん` → 三千 / 3000 / ３０００).
/// 2. Number + counter — reading splits into `<number><counter>` where the
///    counter suffix is a `名詞,接尾,助数詞` POS in the lattice (e.g.
///    `さんぜんえん` → 三千円 / 3000円 / ３０００円). Counter detection uses
///    the dictionary's POS tagging via `ConnectionMatrix::is_counter`, so the
///    counter set extends automatically as the dictionary grows.
pub(crate) struct NumericRewriter<'a> {
    pub lattice: Option<&'a Lattice>,
    pub connection: Option<&'a ConnectionMatrix>,
}

impl Rewriter for NumericRewriter<'_> {
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
        let mut candidates = Vec::new();

        if let Some(n) = numeric::parse_japanese_number(reading) {
            let best_cost = paths.iter().map(|p| p.viterbi_cost).min().unwrap_or(0);
            let base_cost = worst_cost(paths).saturating_add(5000);

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
        }

        if let (Some(lattice), Some(conn)) = (self.lattice, self.connection) {
            self.append_counter_candidates(lattice, conn, paths, reading, &mut candidates);
        }

        candidates
    }
}

impl NumericRewriter<'_> {
    /// Scan the lattice for counter (助数詞) nodes ending at the reading's tail.
    /// For each unique counter surface, try to parse the kana prefix as a number,
    /// and emit kanji / half-width / full-width counter compounds.
    ///
    /// Counter ambiguity is resolved by the counter node's own word cost: the
    /// cheapest counter at the position anchors at `best_cost - 500` (so the
    /// kanji compound surfaces above the existing top-1) and pricier counter
    /// homophones get penalised by their cost difference. This mirrors what
    /// Viterbi would do if a `<kanji_number><counter>` segmentation were
    /// representable in the lattice.
    fn append_counter_candidates(
        &self,
        lattice: &Lattice,
        conn: &ConnectionMatrix,
        paths: &[ScoredPath],
        reading: &str,
        out: &mut Vec<ScoredPath>,
    ) {
        let char_count = reading.chars().count();
        if char_count < 2 {
            return;
        }
        let byte_offsets: Vec<usize> = reading
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(reading.len()))
            .collect();
        let Some(end_nodes) = lattice.nodes_by_end.get(char_count) else {
            return;
        };

        // Collect the cheapest counter node per surface, skipping pseudo
        // kana-surface entries (e.g. an `えん` counter node whose surface is
        // also `えん` — useful in a kana lattice but never the right thing to
        // pair with a kanji number).
        struct Cand<'a> {
            start: usize,
            surface: &'a str,
            cost: i16,
        }
        let mut by_surface: std::collections::HashMap<&str, Cand<'_>> =
            std::collections::HashMap::new();
        for &idx in end_nodes {
            if !conn.is_counter(lattice.left_id(idx)) {
                continue;
            }
            let counter_start = lattice.start(idx);
            if counter_start == 0 {
                continue;
            }
            let surface = lattice.surface(idx);
            let reading_kana = lattice.reading(idx);
            if surface == reading_kana || surface.chars().all(is_hiragana) {
                continue;
            }
            let cost = lattice.cost(idx);
            by_surface
                .entry(surface)
                .and_modify(|c| {
                    if cost < c.cost {
                        c.cost = cost;
                        c.start = counter_start;
                    }
                })
                .or_insert(Cand {
                    start: counter_start,
                    surface,
                    cost,
                });
        }
        if by_surface.is_empty() {
            return;
        }

        let cheapest = by_surface.values().map(|c| c.cost).min().unwrap_or(0);
        let best_cost = paths.iter().map(|p| p.viterbi_cost).min().unwrap_or(0);
        let base_cost = worst_cost(paths).saturating_add(5000);
        // Discount keeps the most-likely number+counter compound above the
        // current Viterbi top-1, since this segmentation isn't representable
        // in the lattice (no `三千` dictionary entry).
        let kanji_anchor = best_cost.saturating_sub(500);

        for cand in by_surface.values() {
            let prefix = &reading[..byte_offsets[cand.start]];
            let Some(n) = numeric::parse_japanese_number(prefix) else {
                continue;
            };
            let cost_offset = (cand.cost - cheapest) as i64;

            let kanji = format!("{}{}", numeric::to_kanji(n), cand.surface);
            out.push(ScoredPath::single(
                reading.to_string(),
                kanji,
                kanji_anchor.saturating_add(cost_offset),
            ));

            let halfwidth = format!("{}{}", numeric::to_halfwidth(n), cand.surface);
            out.push(ScoredPath::single(
                reading.to_string(),
                halfwidth,
                base_cost.saturating_add(cost_offset),
            ));

            let fullwidth = format!("{}{}", numeric::to_fullwidth(n), cand.surface);
            out.push(ScoredPath::single(
                reading.to_string(),
                fullwidth,
                base_cost.saturating_add(1).saturating_add(cost_offset),
            ));
        }
    }
}
