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
