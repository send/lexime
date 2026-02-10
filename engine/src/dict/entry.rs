use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub surface: String,
    pub cost: i16,
    pub left_id: u16,
    pub right_id: u16,
}
