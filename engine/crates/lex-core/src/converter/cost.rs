use crate::dict::connection::ConnectionMatrix;
use crate::settings::settings;
use crate::unicode::{is_hiragana, is_kanji, is_katakana, is_latin};

use super::lattice::Lattice;

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

impl CostFunction for DefaultCostFunction<'_> {
    fn word_cost(&self, lattice: &Lattice, idx: usize) -> i64 {
        let seg_penalty = settings().cost.segment_penalty;
        let is_fw = self
            .conn
            .map(|c| c.is_function_word(lattice.left_id(idx)))
            .unwrap_or(false);
        let penalty = if is_fw { seg_penalty / 2 } else { seg_penalty };
        lattice.cost(idx) as i64 + penalty
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
