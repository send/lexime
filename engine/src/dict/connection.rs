use memmap2::Mmap;

pub(super) const MAGIC: &[u8; 4] = b"LXCX";
pub(super) const V2_HEADER_SIZE: usize = 4 + 1 + 2 + 2 + 2; // magic + version + num_ids + fw_min + fw_max
pub(super) const V1_HEADER_SIZE: usize = 4 + 1 + 2; // magic + v1 + num_ids (for backward compat)
                                                    // V3 header: V2 header + roles[num_ids]
                                                    // header_size is computed dynamically: V2_HEADER_SIZE + num_ids

/// Backing storage for cost data: either owned or memory-mapped.
pub(super) enum CostStorage {
    Owned(Vec<i16>),
    Mapped(Mmap),
}

/// A connection cost matrix mapping (left_id, right_id) → cost.
/// Used by the Viterbi algorithm to score morpheme transitions.
pub struct ConnectionMatrix {
    pub(super) num_ids: u16,
    pub(super) fw_min: u16,
    pub(super) fw_max: u16,
    pub(super) roles: Vec<u8>,
    pub(super) header_size: usize,
    pub(super) storage: CostStorage,
}

impl ConnectionMatrix {
    /// Create a new owned ConnectionMatrix.
    pub(super) fn new_owned(
        num_ids: u16,
        fw_min: u16,
        fw_max: u16,
        roles: Vec<u8>,
        costs: Vec<i16>,
    ) -> Self {
        Self {
            num_ids,
            fw_min,
            fw_max,
            roles,
            header_size: V2_HEADER_SIZE,
            storage: CostStorage::Owned(costs),
        }
    }

    /// Look up the connection cost between two morphemes.
    /// Index: left_id * num_ids + right_id. Max index with u16 is ~4.3B,
    /// which fits in usize on 64-bit targets. Out-of-bounds returns 0.
    pub fn cost(&self, left_id: u16, right_id: u16) -> i16 {
        let idx = (left_id as usize)
            .saturating_mul(self.num_ids as usize)
            .saturating_add(right_id as usize);
        match &self.storage {
            CostStorage::Owned(costs) => {
                debug_assert!(
                    idx < costs.len(),
                    "connection matrix OOB: left_id={left_id}, right_id={right_id}, num_ids={}",
                    self.num_ids
                );
                costs.get(idx).copied().unwrap_or(0)
            }
            CostStorage::Mapped(mmap) => {
                let byte_offset = self.header_size + idx * 2;
                debug_assert!(
                    byte_offset + 2 <= mmap.len(),
                    "connection matrix mmap OOB: left_id={left_id}, right_id={right_id}, num_ids={}",
                    self.num_ids
                );
                mmap.get(byte_offset..byte_offset + 2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .unwrap_or(0)
            }
        }
    }

    /// Number of morpheme IDs in this matrix.
    pub fn num_ids(&self) -> u16 {
        self.num_ids
    }

    /// Function-word POS ID range (lower bound, inclusive).
    pub fn fw_min(&self) -> u16 {
        self.fw_min
    }

    /// Function-word POS ID range (upper bound, inclusive).
    pub fn fw_max(&self) -> u16 {
        self.fw_max
    }

    /// Check whether a POS ID falls in the function-word range (助詞/助動詞).
    /// Returns `false` when no range is set (both 0).
    pub fn is_function_word(&self, id: u16) -> bool {
        self.fw_min != 0 && self.fw_min <= id && id <= self.fw_max
    }

    /// Get the morpheme role for a POS ID.
    /// Returns 0 (ContentWord) if roles data is not available (V1/V2 matrices).
    pub fn role(&self, id: u16) -> u8 {
        self.roles.get(id as usize).copied().unwrap_or(0)
    }

    /// Check whether a POS ID is a suffix (接尾, role == 2).
    pub fn is_suffix(&self, id: u16) -> bool {
        self.role(id) == 2
    }

    /// Check whether a POS ID is a prefix (接頭詞, role == 3).
    pub fn is_prefix(&self, id: u16) -> bool {
        self.role(id) == 3
    }
}
