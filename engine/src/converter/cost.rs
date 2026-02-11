use crate::dict::connection::ConnectionMatrix;
use crate::unicode::{is_hiragana, is_kanji, is_katakana, is_latin};

use super::lattice::LatticeNode;

/// Per-segment penalty added to each node's word cost.
/// Discourages the Viterbi algorithm from choosing paths with many short segments
/// over paths with fewer, longer (and usually more natural) segments.
pub const SEGMENT_PENALTY: i64 = 5000;

/// Bonus subtracted from word cost for mixed-script surfaces (kanji + kana).
/// Verb conjugations like 通っ, 食べ, 走る contain both kanji and kana — these are
/// the primary conversion targets for an IME and should be preferred.
const MIXED_SCRIPT_BONUS: i64 = 3000;

/// Penalty added to word cost when the surface is all katakana.
/// Katakana noun forms (e.g. タラ) often have low dictionary costs but are rarely
/// the intended conversion for grammatical words (e.g. たら as 助動詞).
const KATAKANA_PENALTY: i64 = 5000;

/// Penalty for surfaces containing ASCII/Latin characters.
/// SudachiDict includes English surface forms (e.g. death for です, tie for たい)
/// with low costs intended for morphological analysis, not IME conversion.
const LATIN_PENALTY: i64 = 20000;

/// Cost adjustment based on the surface script.
/// - Mixed-script (kanji+kana, e.g. 通っ, 食べる): bonus (negative)
/// - Contains Latin/ASCII (e.g. death, tie, thai): heavy penalty
/// - All-katakana (e.g. タラ, オッ): penalty (positive)
/// - Otherwise (pure kanji, hiragana, etc.): no adjustment
pub fn script_cost(surface: &str) -> i64 {
    if surface.chars().any(is_latin) {
        return LATIN_PENALTY;
    }
    let has_kanji = surface.chars().any(is_kanji);
    let has_kana = surface.chars().any(|c| is_hiragana(c) || is_katakana(c));
    if has_kanji && has_kana {
        -MIXED_SCRIPT_BONUS
    } else if !surface.is_empty() && surface.chars().all(is_katakana) {
        KATAKANA_PENALTY
    } else {
        0
    }
}

/// Trait for scoring lattice paths during Viterbi search.
pub trait CostFunction: Send + Sync {
    fn word_cost(&self, node: &LatticeNode) -> i64;
    fn transition_cost(&self, prev: &LatticeNode, next: &LatticeNode) -> i64;
    fn bos_cost(&self, node: &LatticeNode) -> i64;
    fn eos_cost(&self, node: &LatticeNode) -> i64;
}

/// Look up connection cost between two IDs, returning 0 if no matrix is provided.
pub fn conn_cost(conn: Option<&ConnectionMatrix>, left: u16, right: u16) -> i64 {
    conn.map(|c| c.cost(left, right) as i64).unwrap_or(0)
}

/// Default cost function using word costs and optional connection matrix.
pub struct DefaultCostFunction<'a> {
    conn: Option<&'a ConnectionMatrix>,
}

impl<'a> DefaultCostFunction<'a> {
    pub fn new(conn: Option<&'a ConnectionMatrix>) -> Self {
        Self { conn }
    }
}

impl CostFunction for DefaultCostFunction<'_> {
    fn word_cost(&self, node: &LatticeNode) -> i64 {
        node.cost as i64 + SEGMENT_PENALTY + script_cost(&node.surface)
    }

    fn transition_cost(&self, prev: &LatticeNode, next: &LatticeNode) -> i64 {
        conn_cost(self.conn, prev.right_id, next.left_id)
    }

    fn bos_cost(&self, node: &LatticeNode) -> i64 {
        conn_cost(self.conn, 0, node.left_id)
    }

    fn eos_cost(&self, node: &LatticeNode) -> i64 {
        conn_cost(self.conn, node.right_id, 0)
    }
}
