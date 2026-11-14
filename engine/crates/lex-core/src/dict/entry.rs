use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub surface: String,
    pub cost: i16,
    pub left_id: u16,
    pub right_id: u16,
    /// Entry comes from the lexime-bundled symbols source. Set by source
    /// alone (never inferred from POS): symbol entries live outside the
    /// lattice — lattice construction neither builds nodes from them nor
    /// counts them toward the single-char connectivity check. user_dict,
    /// Mozc and extras entries are always `false`.
    pub symbol: bool,
}

impl DictEntry {
    /// Order of the entries of one reading (an exact `lookup`): lattice words
    /// by cost, then symbols by cost — a symbol's cost is its row index among
    /// the same reading's symbol rows, comparable only with other symbols.
    /// Sort stably by this key.
    pub fn lookup_order(&self) -> (bool, i16) {
        (self.symbol, self.cost)
    }
}
