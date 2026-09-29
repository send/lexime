use crate::dict::connection::ConnectionMatrix;
use crate::numeric;
use crate::unicode::{hiragana_to_katakana, is_hiragana, is_kanji, is_katakana};

use super::cost::{conn_cost, node_cost, score_path};
use super::lattice::Lattice;
use super::reranker::FeaturePricer;
use super::viterbi::{PathOrigin, RichSegment, ScoredPath};

/// A rewriter that generates new candidates from the N-best list.
///
/// Implementations return new candidates without mutating the input.
/// Deduplication and cost-ordered insertion are handled by `run_rewriters`.
pub(crate) trait Rewriter {
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath>;
}

/// Worst (highest) pre-history Viterbi cost among paths, or 0 if empty.
///
/// Uses `pre_history_cost` so that rewriters running after `history_rerank`
/// derive fallback costs from the intrinsic Viterbi cost, not from a
/// history-boosted (possibly large-negative) cost.
fn worst_cost(paths: &[ScoredPath]) -> i64 {
    paths
        .iter()
        .map(|p| p.pre_history_cost())
        .max()
        .unwrap_or(0)
}

/// Where a group of rewriters runs, which decides what it may override.
/// Neither stage moves a learned index 0: history reranking put the
/// cheapest learned path there, and a learned surface outranks policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RewriteStage {
    /// Alternatives to the model's ranking, before history: never touch
    /// index 0.
    Model,
    /// Rewriters whose policy is to override the model, index 0 included:
    /// NumericRewriter puts a number compound first on purpose (#239) —
    /// unless index 0 is a surface the user has learned for this reading,
    /// which outranks any policy.
    Override,
}

impl RewriteStage {
    /// Leading entries of `paths` this stage may neither displace nor
    /// reprice: the Model stage never touches index 0; the Override stage
    /// leaves it alone only when it is learned (`whole_path_boost > 0`,
    /// placed there by history reranking).
    fn frozen_prefix(self, paths: &[ScoredPath]) -> usize {
        match self {
            Self::Model => 1,
            Self::Override => usize::from(paths.first().is_some_and(|p| p.whole_path_boost > 0)),
        }
    }

    /// Whether a candidate goes before entries of equal cost. A Model-stage
    /// candidate is an alternative, so it follows the path it ties (an
    /// offer at its source's price stays below the source); an Override
    /// candidate is policy, so it leads (a number compound at the anchor's
    /// price takes index 0, #239).
    fn precedes_ties(self) -> bool {
        match self {
            Self::Model => false,
            Self::Override => true,
        }
    }
}

/// Run a stage's rewriters, resolving duplicates and inserting in cost
/// order below the stage's frozen prefix. Every rewriter generates from the
/// stage's input list before anything is inserted, so no rewriter sees
/// another's output: an offer is never a source, and the rewriters' order
/// does not change what they generate.
pub(crate) fn run_rewriters(
    rewriters: &[&dyn Rewriter],
    paths: &mut Vec<ScoredPath>,
    reading: &str,
    stage: RewriteStage,
) {
    // Decided on the stage's input, so inserting cannot change it.
    let fp = stage.frozen_prefix(paths);
    let candidates: Vec<ScoredPath> = rewriters
        .iter()
        .flat_map(|rw| rw.generate(paths, reading))
        .collect();
    let rescue = PathOrigin::HiraganaVariant;
    let rescue_keys: Vec<String> = candidates
        .iter()
        .filter(|c| c.priced_by == rescue)
        .map(ScoredPath::surface_key)
        .collect();
    for candidate in candidates {
        let key = candidate.surface_key();
        match paths.iter().position(|p| p.surface_key_eq(&key)) {
            None => insert_by_cost(paths, candidate, stage, fp),
            Some(i) => {
                if let Some(resolved) = resolve_duplicate(stage, candidate, &paths[i], i, fp) {
                    paths.remove(i);
                    insert_by_cost(paths, resolved, stage, fp);
                }
            }
        }
    }
    // The kana rescue's surface stays marked as the rescue whichever rewriter
    // priced it — the mark keeps it on the list (#263). Decided once the
    // stage has settled, so it does not depend on the rewriters' order; set
    // in place, so a mark does not move the path. A model price stays the
    // model's.
    for p in paths.iter_mut() {
        if !p.priced_by.is_model() && rescue_keys.iter().any(|k| p.surface_key_eq(k)) {
            p.priced_by = rescue;
        }
    }
}

/// Insert before the first entry (past the frozen prefix `fp`) that costs more,
/// or at least as much when the stage precedes ties. A linear scan, so the
/// position is defined even where the list is not sorted (the Viterbi best
/// is re-inserted at index 1 after history reranking).
fn insert_by_cost(paths: &mut Vec<ScoredPath>, path: ScoredPath, stage: RewriteStage, fp: usize) {
    let fp = fp.min(paths.len());
    let precedes = stage.precedes_ties();
    let pos = fp
        + paths[fp..]
            .iter()
            .position(|p| {
                p.viterbi_cost > path.viterbi_cost
                    || (precedes && p.viterbi_cost == path.viterbi_cost)
            })
            .unwrap_or(paths.len() - fp);
    paths.insert(pos, path);
}

/// Decide what a candidate does to an existing path with the same surface.
/// Returns the path to re-insert in the existing one's place, or `None` to
/// leave the list as it is.
///
/// Every rewriter price is a standing contract (an offer ≤ source+2000, the
/// kana rescue ≤ best+4000, Numeric #239), so the cheaper price wins and
/// `priced_by` names whoever set it (the kana rescue's mark is settled after
/// the stage, in [`run_rewriters`]). The segments come from the lattice side
/// when there is one — committing the surface then records per-segment
/// history (#271) — else the finer segmentation.
fn resolve_duplicate(
    stage: RewriteStage,
    candidate: ScoredPath,
    existing: &ScoredPath,
    i: usize,
    fp: usize,
) -> Option<ScoredPath> {
    if i < fp {
        return None;
    }
    // The Model stage runs before history: there is no boost to keep.
    debug_assert!(
        stage == RewriteStage::Override
            || (candidate.history_boost == 0
                && existing.history_boost == 0
                && candidate.whole_path_boost == 0
                && existing.whole_path_boost == 0)
    );
    let cheaper = candidate.viterbi_cost < existing.viterbi_cost;
    let take_segments = !existing.origin.is_lattice_path()
        && (candidate.origin.is_lattice_path()
            || candidate.segments.len() > existing.segments.len());
    if !cheaper && !take_segments {
        return None;
    }
    // Taking the candidate's segments but not its price would keep a price
    // the resolved path's boost no longer describes; only the Model stage
    // (no boosts yet) can.
    debug_assert!(cheaper || stage == RewriteStage::Model);
    let (cost, priced_by) = if cheaper {
        (candidate.viterbi_cost, candidate.priced_by)
    } else {
        (existing.viterbi_cost, existing.priced_by)
    };
    let mut resolved = if take_segments {
        candidate
    } else {
        existing.clone()
    };
    resolved.viterbi_cost = cost;
    resolved.priced_by = priced_by;
    // An Override price replaces the model's, so the boost it carried no
    // longer describes it (F6). (The Model stage runs before history.)
    resolved.history_boost = 0;
    resolved.whole_path_boost = 0;
    Some(resolved)
}

/// A variant of `source` with segment `i` replaced by `segment`, priced as
/// rerank would price it but within `[source, source + OFFER_CAP]`: a
/// spelling the dictionary prices far above another (kana for 補助動詞 /
/// 形式名詞) stays next to it, and an offer never undercuts the path it
/// varies (one priced under the best would take #1 once history re-sorts).
/// `model` is the variant's Viterbi price (`score_path`).
fn offer(
    source: &ScoredPath,
    i: usize,
    segment: RichSegment,
    model: i64,
    pricer: &FeaturePricer<'_>,
    origin: PathOrigin,
) -> ScoredPath {
    let mut segments = source.segments.clone();
    segments[i] = segment;
    let mut path = ScoredPath::new(segments, 0, origin);
    let src = source.pre_history_cost();
    path.viterbi_cost =
        (model + pricer.adjustment(&path)).clamp(src, src.saturating_add(OFFER_CAP));
    path
}

/// How far above its source a variant may be offered.
pub(crate) const OFFER_CAP: i64 = 2000;

/// How far above the best the kana rescue is offered (#263). Cost-gap
/// admission never cuts below this band, so the rescue's surface stays.
pub(crate) const RESCUE_OFFSET: i64 = 4000;

/// The largest Viterbi-price increase a kanji spelling may add over its
/// source and still be offered (したほうがいい's 方: 2357). Measured against the source,
/// not the rest of the list: rerank's structure filter judges a path by the
/// population's best, so a sound variant would come and go with the
/// oversample.
const OFFER_MAX_MODEL_GAP: i64 = 6000;

/// The first 5 multi-segment model-priced paths: the sources both variant
/// rewriters work from. Rewriters see only the stage's input
/// ([`run_rewriters`]), so a source is never an offer or a path an offer
/// repriced, and variants do not chain.
fn offer_sources(
    paths: &[ScoredPath],
    usable: impl Fn(&ScoredPath) -> bool,
) -> impl Iterator<Item = &ScoredPath> {
    paths
        .iter()
        .filter(move |p| p.priced_by.is_model() && p.segments.len() > 1 && usable(p))
        .take(5)
}

/// Whether lattice node `idx` is the same word class as `seg` (content
/// word, function word, …, per the dictionary's roles): a variant respells
/// a word, it does not swap a particle for a noun. No matrix, no roles, so
/// no restriction (fixtures only).
fn same_role(
    conn: Option<&ConnectionMatrix>,
    lattice: &Lattice,
    idx: usize,
    seg: &RichSegment,
) -> bool {
    conn.is_none_or(|c| c.role(lattice.left_id(idx)) == c.role(seg.left_id))
}

/// `(segment index, char span)` for each segment of `path`.
fn segment_spans(path: &ScoredPath) -> impl Iterator<Item = (usize, std::ops::Range<usize>)> + '_ {
    path.segments
        .iter()
        .enumerate()
        .scan(0usize, |pos, (i, seg)| {
            let start = *pos;
            *pos += seg.reading.chars().count();
            Some((i, start..*pos))
        })
}

/// Lattice nodes covering exactly `span` that pass `keep`.
fn nodes_at<'a>(
    lattice: &'a Lattice,
    span: std::ops::Range<usize>,
    keep: impl Fn(usize) -> bool + 'a,
) -> impl Iterator<Item = usize> + 'a {
    lattice
        .nodes_by_start
        .get(span.start)
        .into_iter()
        .flatten()
        .copied()
        .filter(move |&idx| lattice.end(idx) == span.end && keep(idx))
}

/// Among `nodes`, the one whose swap into segment `i` of `source` the
/// Viterbi model prices lowest, with that swap's price change. Only the
/// node and its two connections change, so no path is built to compare.
fn favourite_swap(
    lattice: &Lattice,
    conn: Option<&ConnectionMatrix>,
    source: &ScoredPath,
    i: usize,
    nodes: impl Iterator<Item = usize>,
) -> Option<(usize, i64)> {
    let segs = &source.segments;
    let before = if i == 0 { 0 } else { segs[i - 1].right_id };
    let after = segs.get(i + 1).map_or(0, |s| s.left_id);
    let local = |left: u16, right: u16, cost: i16| {
        node_cost(cost, left, conn) + conn_cost(conn, before, left) + conn_cost(conn, right, after)
    };
    let old = local(segs[i].left_id, segs[i].right_id, segs[i].word_cost);
    nodes
        .map(|idx| {
            let new = local(
                lattice.left_id(idx),
                lattice.right_id(idx),
                lattice.cost(idx),
            );
            (idx, new - old)
        })
        .min_by_key(|&(_, gap)| gap)
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
            PathOrigin::Katakana,
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

        // Anchor to the BEST path, not the worst: the kana variant is the
        // rescue candidate when the cost model kanjifies wrongly (#263), so
        // it must land mid-list where it is visible and history-boostable.
        // Bottom-anchored (worst+5000) it could never be reached once the
        // n-best filled with 20 lattice paths.
        vec![ScoredPath::single(
            combined_reading,
            combined_surface,
            best.pre_history_cost().saturating_add(RESCUE_OFFSET),
            PathOrigin::HiraganaVariant,
        )]
    }
}

/// For the first 5 multi-segment model-priced paths that have a kanji
/// segment, offer variants where one kanji segment is spelled in kana.
///
/// Example: `下|方|が|良い` → `した|方|が|良い`
///
/// The reverse of [`KanjiVariantRewriter`], under the same offer price
/// ([`offer`]) but without its gap bound: kana is always a valid
/// spelling (#263). The kana segment is the lattice's kana node for the span
/// of the same word class ([`same_role`]) when there is one; otherwise —
/// no kana node, or none of that class — the kanji node keeps its POS and
/// cost under its reading (as the kana rescue does for the whole input),
/// offered at the cap.
pub(crate) struct PartialHiraganaRewriter<'a> {
    pub lattice: &'a Lattice,
    pub conn: Option<&'a ConnectionMatrix>,
    pub pricer: &'a FeaturePricer<'a>,
}

impl Rewriter for PartialHiraganaRewriter<'_> {
    fn generate(&self, paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        let replaceable =
            |s: &RichSegment| s.surface != s.reading && !s.surface.chars().all(is_katakana);
        let mut new_paths = Vec::new();
        let mut fallbacks = Vec::new();

        // Select the 5 cheapest paths that can actually yield variants BEFORE
        // limiting: paths with no replaceable segment must not displace a
        // kanji-bearing source.
        for source in offer_sources(paths, |p| p.segments.iter().any(replaceable)) {
            let model = score_path(&source.segments, self.conn);
            for (i, span) in segment_spans(source) {
                if !replaceable(&source.segments[i]) {
                    continue;
                }
                let seg = &source.segments[i];
                let kana = nodes_at(self.lattice, span, |idx| {
                    let s = self.lattice.surface(idx);
                    s == self.lattice.reading(idx)
                        && crate::unicode::is_hiragana_reading(s)
                        && same_role(self.conn, self.lattice, idx, seg)
                });
                match favourite_swap(self.lattice, self.conn, source, i, kana) {
                    Some((idx, gap)) => new_paths.push(offer(
                        source,
                        i,
                        self.lattice.to_rich_segment(idx),
                        model + gap,
                        self.pricer,
                        PathOrigin::PartialHiragana,
                    )),
                    None => {
                        let mut segments = source.segments.clone();
                        segments[i].surface = segments[i].reading.clone();
                        fallbacks.push(ScoredPath::new(
                            segments,
                            source.pre_history_cost().saturating_add(OFFER_CAP),
                            PathOrigin::PartialHiragana,
                        ));
                    }
                }
            }
        }
        // A fallback relabels a kanji node; where another source spells the
        // same surface with real kana nodes, those segments are the ones to
        // keep (dedup would otherwise keep whichever came first, since both
        // are PartialHiragana).
        fallbacks.retain(|f| {
            let key = f.surface_key();
            !new_paths.iter().any(|p| p.surface_key_eq(&key))
        });
        new_paths.extend(fallbacks);

        new_paths
    }
}

/// For the first 5 multi-segment model paths, offer variants where one kana
/// segment (2+ chars) is spelled with the kanji node the lattice has for
/// exactly its span and the same word class — the one Viterbi (dictionary
/// and connection costs) prices lowest.
///
/// Example: `あった|ほう|が` → `あった|方|が`, `し|て|ください` → `し|て|下さい`
///
/// Variants are offers ([`offer`]): a kanji spelling the model prices
/// more than `OFFER_MAX_MODEL_GAP` above its source is not offered; one it prices far above but within the bound (`して下さい`) is
/// offered next to its source. One offer per segment, so homophones do not
/// fill the list at the same capped price.
pub(crate) struct KanjiVariantRewriter<'a> {
    pub lattice: &'a Lattice,
    pub conn: Option<&'a ConnectionMatrix>,
    pub pricer: &'a FeaturePricer<'a>,
}

impl Rewriter for KanjiVariantRewriter<'_> {
    fn generate(&self, paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        let mut new_paths = Vec::new();

        // Multi-segment paths only: a kanji span is an existing segment's
        // span, never an arbitrary offset inside a single-segment kana run.
        // Kana segments of 2+ chars: single-char ones are almost always
        // function morphemes (し, た, な, が).
        let eligible = |seg: &RichSegment| {
            seg.surface == seg.reading
                && crate::unicode::is_hiragana_reading(&seg.surface)
                && seg.reading.chars().count() >= 2
        };
        // Select the 5 cheapest paths that can yield variants BEFORE
        // limiting, as Partial does.
        for source in offer_sources(paths, |p| p.segments.iter().any(eligible)) {
            let model = score_path(&source.segments, self.conn);
            for (i, span) in segment_spans(source) {
                let seg = &source.segments[i];
                if !eligible(seg) {
                    continue;
                }
                let kanji = nodes_at(self.lattice, span, |idx| {
                    self.lattice.surface(idx).chars().any(is_kanji)
                        && same_role(self.conn, self.lattice, idx, seg)
                });
                if let Some((idx, gap)) = favourite_swap(self.lattice, self.conn, source, i, kanji)
                    .filter(|&(_, gap)| gap <= OFFER_MAX_MODEL_GAP)
                {
                    new_paths.push(offer(
                        source,
                        i,
                        self.lattice.to_rich_segment(idx),
                        model + gap,
                        self.pricer,
                        PathOrigin::KanjiVariant,
                    ));
                }
            }
        }

        new_paths
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
///
/// Number compounds are priced from `anchor` — the rerank best's cost
/// before history — not from the current list, so a cheaper path added by
/// another rewriter cannot lower them past a learned #1.
pub(crate) struct NumericRewriter<'a> {
    pub lattice: Option<&'a Lattice>,
    pub connection: Option<&'a ConnectionMatrix>,
    pub anchor: i64,
}

impl Rewriter for NumericRewriter<'_> {
    fn generate(&self, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
        let mut candidates = Vec::new();

        if let Some(n) = numeric::parse_japanese_number(reading) {
            let best_cost = self.anchor;
            let base_cost = worst_cost(paths).saturating_add(5000);

            // Kanji candidate
            let kanji = numeric::to_kanji(n);
            let is_compound = kanji.chars().count() > 1;
            let kanji_cost = if is_compound { best_cost } else { base_cost };
            candidates.push(ScoredPath::single(
                reading.to_string(),
                kanji,
                kanji_cost,
                PathOrigin::Numeric,
            ));

            // Half-width Arabic digits
            let halfwidth = numeric::to_halfwidth(n);
            candidates.push(ScoredPath::single(
                reading.to_string(),
                halfwidth,
                base_cost,
                PathOrigin::Numeric,
            ));

            // Full-width Arabic digits
            let fullwidth = numeric::to_fullwidth(n);
            candidates.push(ScoredPath::single(
                reading.to_string(),
                fullwidth,
                base_cost.saturating_add(1),
                PathOrigin::Numeric,
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

        // HashMap iteration is nondeterministic; sort by (cost, surface) so
        // tied counter candidates emit in a stable, reproducible order.
        let mut cands: Vec<Cand<'_>> = by_surface.into_values().collect();
        cands.sort_by(|a, b| a.cost.cmp(&b.cost).then_with(|| a.surface.cmp(b.surface)));

        let cheapest = cands.first().map(|c| c.cost).unwrap_or(0);
        let best_cost = self.anchor;
        let base_cost = worst_cost(paths).saturating_add(5000);
        // Discount keeps the most-likely number+counter compound above the
        // current Viterbi top-1, since this segmentation isn't representable
        // in the lattice (no `三千` dictionary entry).
        let kanji_anchor = best_cost.saturating_sub(500);

        for cand in &cands {
            let prefix = &reading[..byte_offsets[cand.start]];
            let Some(n) = numeric::parse_japanese_number(prefix) else {
                continue;
            };
            // Widen to i64 before subtraction: i16 - i16 can overflow if the
            // dictionary contains extreme positive/negative costs.
            let cost_offset = cand.cost as i64 - cheapest as i64;

            let kanji = format!("{}{}", numeric::to_kanji(n), cand.surface);
            out.push(ScoredPath::single(
                reading.to_string(),
                kanji,
                kanji_anchor.saturating_add(cost_offset),
                PathOrigin::Numeric,
            ));

            let halfwidth = format!("{}{}", numeric::to_halfwidth(n), cand.surface);
            out.push(ScoredPath::single(
                reading.to_string(),
                halfwidth,
                base_cost.saturating_add(cost_offset),
                PathOrigin::Numeric,
            ));

            let fullwidth = format!("{}{}", numeric::to_fullwidth(n), cand.surface);
            out.push(ScoredPath::single(
                reading.to_string(),
                fullwidth,
                base_cost.saturating_add(1).saturating_add(cost_offset),
                PathOrigin::Numeric,
            ));
        }
    }
}
