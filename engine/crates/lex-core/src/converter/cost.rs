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
/// Hybrid design: `word_cost` receives `(&Lattice, usize)` because
/// `PrefixConstrainedCost` needs full node inspection (start, end,
/// reading, surface).  The other three methods take raw IDs — both
/// implementations only ever read `left_id` / `right_id`, so passing
/// the Lattice would be wasteful (especially for `transition_cost`,
/// the most frequent call at O(P*Q) per position).
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
    let seg_penalty = settings().cost.segment_penalty;
    let is_fw = conn.is_some_and(|c| c.is_function_word(left_id));
    let penalty = if is_fw { seg_penalty / 2 } else { seg_penalty };
    word_cost as i64 + penalty
}

/// Price a segment sequence exactly as `DefaultCostFunction` prices the same
/// path in Viterbi: node costs + BOS + transitions + EOS.
pub(crate) fn score_path(segments: &[RichSegment], conn: Option<&ConnectionMatrix>) -> i64 {
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return 0;
    };
    let nodes: i64 = segments
        .iter()
        .map(|s| node_cost(s.word_cost, s.left_id, conn))
        .sum();
    let transitions: i64 = segments
        .windows(2)
        .map(|w| conn_cost(conn, w[0].right_id, w[1].left_id))
        .sum();
    nodes + conn_cost(conn, 0, first.left_id) + transitions + conn_cost(conn, last.right_id, 0)
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
