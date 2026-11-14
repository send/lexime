use crate::dict::connection::ConnectionMatrix;
use crate::settings::settings;
use crate::unicode::{is_hiragana, is_kanji, is_katakana, is_latin};

use super::lattice::Lattice;
use super::viterbi::RichSegment;

/// Cost adjustment based on the surface script.
/// - Mixed-script (kanji+kana, e.g. 通っ, 食べる): bonus (negative)
/// - Pure kanji (e.g. 方, 気, 人): small bonus (negative)
/// - Contains Latin/ASCII (e.g. death, tie, thai): heavy penalty
/// - All-katakana (e.g. タラ, オッ): penalty (positive)
/// - Otherwise (pure hiragana, etc.): no adjustment
pub fn script_cost(surface: &str, reading_chars: usize) -> i64 {
    let s = settings();
    let mut has_kanji = false;
    let mut has_kana = false;
    let mut all_katakana = !surface.is_empty();
    for c in surface.chars() {
        if is_latin(c) {
            return s.cost.latin_penalty;
        }
        if is_kanji(c) {
            has_kanji = true;
        }
        if is_hiragana(c) || is_katakana(c) {
            has_kana = true;
        }
        if !is_katakana(c) {
            all_katakana = false;
        }
    }
    let scale = reading_chars.min(2) as i64;
    if has_kanji && has_kana {
        -s.cost.mixed_script_bonus * scale / 3
    } else if has_kanji {
        -s.cost.pure_kanji_bonus * scale / 3
    } else if all_katakana {
        s.cost.katakana_penalty
    } else {
        0
    }
}

/// Trait for scoring lattice paths during Viterbi search.
///
/// `DefaultCostFunction` is the one implementation, and its `word_cost` reads
/// only the node's cost and `left_id`; the `(&Lattice, usize)` signature is
/// left from a removed prefix-constrained cost that looked at the whole node.
/// The other three methods take raw IDs because `transition_cost` is the
/// most frequent call (O(P*Q) per position).
pub(crate) trait CostFunction: Send + Sync {
    fn word_cost(&self, lattice: &Lattice, idx: usize) -> i64;
    fn transition_cost(&self, prev_right_id: u16, next_left_id: u16) -> i64;
    fn bos_cost(&self, left_id: u16) -> i64;
    fn eos_cost(&self, right_id: u16) -> i64;
}

/// Look up connection cost between two IDs, returning 0 if no matrix is provided.
pub fn conn_cost(conn: Option<&ConnectionMatrix>, left: u16, right: u16) -> i64 {
    conn.map(|c| c.cost(left, right) as i64).unwrap_or(0)
}

/// Default cost function using word costs and optional connection matrix.
pub(crate) struct DefaultCostFunction<'a> {
    conn: Option<&'a ConnectionMatrix>,
}

impl<'a> DefaultCostFunction<'a> {
    pub fn new(conn: Option<&'a ConnectionMatrix>) -> Self {
        Self { conn }
    }
}

/// Cost of one node on a path: its dictionary cost plus the segment
/// penalty (halved for function words). The single definition Viterbi and
/// [`score_path`] both use.
pub(crate) fn node_cost(word_cost: i16, left_id: u16, conn: Option<&ConnectionMatrix>) -> i64 {
    let (word, penalty) = node_terms(word_cost, left_id, conn);
    word + penalty
}

/// The two terms of [`node_cost`]: the dictionary cost and the segment
/// penalty (halved for function words).
pub(crate) fn node_terms(
    word_cost: i16,
    left_id: u16,
    conn: Option<&ConnectionMatrix>,
) -> (i64, i64) {
    let seg_penalty = settings().cost.segment_penalty;
    let is_fw = conn.is_some_and(|c| c.is_function_word(left_id));
    let penalty = if is_fw { seg_penalty / 2 } else { seg_penalty };
    (word_cost as i64, penalty)
}

/// What one segment adds to [`score_path`]: its node's two terms and the
/// transition into it (from BOS for the first segment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentTerms {
    pub word_cost: i64,
    pub segment_penalty: i64,
    pub transition_in: i64,
}

/// The per-segment terms of [`score_path`], in path order. With
/// [`eos_term`] they are everything it sums.
pub(crate) fn segment_terms<'a>(
    segments: &'a [RichSegment],
    conn: Option<&'a ConnectionMatrix>,
) -> impl Iterator<Item = SegmentTerms> + 'a {
    segments.iter().enumerate().map(move |(i, s)| {
        let (word_cost, segment_penalty) = node_terms(s.word_cost, s.left_id, conn);
        let prev_right = i.checked_sub(1).map_or(0, |j| segments[j].right_id);
        SegmentTerms {
            word_cost,
            segment_penalty,
            transition_in: conn_cost(conn, prev_right, s.left_id),
        }
    })
}

/// The transition from the last segment to EOS (0 for an empty path).
pub(crate) fn eos_term(segments: &[RichSegment], conn: Option<&ConnectionMatrix>) -> i64 {
    segments
        .last()
        .map_or(0, |last| conn_cost(conn, last.right_id, 0))
}

/// Price a segment sequence exactly as `DefaultCostFunction` prices the same
/// path in Viterbi: node costs + BOS + transitions + EOS.
pub(crate) fn score_path(segments: &[RichSegment], conn: Option<&ConnectionMatrix>) -> i64 {
    segment_terms(segments, conn)
        .map(|t| t.word_cost + t.segment_penalty + t.transition_in)
        .sum::<i64>()
        + eos_term(segments, conn)
}

impl CostFunction for DefaultCostFunction<'_> {
    fn word_cost(&self, lattice: &Lattice, idx: usize) -> i64 {
        node_cost(lattice.cost(idx), lattice.left_id(idx), self.conn)
    }

    fn transition_cost(&self, prev_right_id: u16, next_left_id: u16) -> i64 {
        conn_cost(self.conn, prev_right_id, next_left_id)
    }

    fn bos_cost(&self, left_id: u16) -> i64 {
        conn_cost(self.conn, 0, left_id)
    }

    fn eos_cost(&self, right_id: u16) -> i64 {
        conn_cost(self.conn, right_id, 0)
    }
}
