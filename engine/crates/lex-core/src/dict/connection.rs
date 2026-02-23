use memmap2::Mmap;

pub(super) const MAGIC: &[u8; 4] = b"LXCX";
pub(super) const VERSION: u8 = 3;
/// Fixed header size before roles array: magic(4) + version(1) + num_ids(2) + fw_min(2) + fw_max(2).
pub(super) const FIXED_HEADER_SIZE: usize = 4 + 1 + 2 + 2 + 2;

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
    /// Create a new owned ConnectionMatrix (V3 format).
    ///
    /// `roles` is padded with zeros to `num_ids` length if shorter.
    pub(crate) fn new_owned(
        num_ids: u16,
        fw_min: u16,
        fw_max: u16,
        mut roles: Vec<u8>,
        costs: Vec<i16>,
    ) -> Self {
        roles.resize(num_ids as usize, 0);
        Self {
            num_ids,
            fw_min,
            fw_max,
            roles,
            header_size: FIXED_HEADER_SIZE + num_ids as usize,
            storage: CostStorage::Owned(costs),
        }
    }

    /// Look up the connection cost between two morphemes.
    /// Index: left_id * num_ids + right_id. Out-of-bounds returns 0.
    pub fn cost(&self, left_id: u16, right_id: u16) -> i16 {
        let idx = (left_id as usize)
            .saturating_mul(self.num_ids as usize)
            .saturating_add(right_id as usize);
        match &self.storage {
            CostStorage::Owned(costs) => costs.get(idx).copied().unwrap_or(0),
            CostStorage::Mapped(mmap) => {
                let byte_offset = self.header_size + idx * 2;
                mmap.get(byte_offset..byte_offset + 2)
                    .map(|b| i16::from_ne_bytes([b[0], b[1]]))
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
    /// Returns 0 (ContentWord) for IDs beyond the roles vector.
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

    /// Check whether a POS ID is non-independent (非自立, role == 4).
    pub fn is_non_independent(&self, id: u16) -> bool {
        self.role(id) == 4
    }
}
