use std::fs::{self, File};
use std::path::Path;

use memmap2::Mmap;

use super::DictError;

const MAGIC: &[u8; 4] = b"LXCX";
const V2_HEADER_SIZE: usize = 4 + 1 + 2 + 2 + 2; // magic + version + num_ids + fw_min + fw_max
const V1_HEADER_SIZE: usize = 4 + 1 + 2; // magic + v1 + num_ids (for backward compat)
                                         // V3 header: V2 header + roles[num_ids]
                                         // header_size is computed dynamically: V2_HEADER_SIZE + num_ids

/// Backing storage for cost data: either owned or memory-mapped.
enum CostStorage {
    Owned(Vec<i16>),
    Mapped(Mmap),
}

/// A connection cost matrix mapping (left_id, right_id) → cost.
/// Used by the Viterbi algorithm to score morpheme transitions.
pub struct ConnectionMatrix {
    num_ids: u16,
    fw_min: u16,
    fw_max: u16,
    roles: Vec<u8>,
    header_size: usize,
    storage: CostStorage,
}

impl ConnectionMatrix {
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

    /// Build from a text file.
    ///
    /// Supports two formats (auto-detected):
    /// - **Mozc**: Line 1 is `num_ids` (or `num_left num_right`), then one cost per line.
    /// - **MeCab**: Line 1 is `num_left num_right`, then `right_id left_id cost` per line.
    pub fn from_text(text: &str) -> Result<Self, DictError> {
        let mut lines = text.lines().peekable();

        let header = lines
            .next()
            .ok_or_else(|| DictError::Parse("empty file".to_string()))?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        let num_ids: u16 = match parts.len() {
            1 => parts[0]
                .parse()
                .map_err(|e| DictError::Parse(format!("invalid num_ids: {e}")))?,
            2 => {
                let nl: u16 = parts[0]
                    .parse()
                    .map_err(|e| DictError::Parse(format!("invalid num_left: {e}")))?;
                let nr: u16 = parts[1]
                    .parse()
                    .map_err(|e| DictError::Parse(format!("invalid num_right: {e}")))?;
                if nl != nr {
                    return Err(DictError::Parse(format!(
                        "num_left ({nl}) != num_right ({nr})"
                    )));
                }
                nl
            }
            _ => {
                return Err(DictError::Parse(format!(
                    "expected 1 or 2 values in header, got {}",
                    parts.len()
                )));
            }
        };

        let expected = num_ids as usize * num_ids as usize;

        // Auto-detect format: skip empty lines then peek at first data line
        while lines.peek().is_some_and(|line| line.trim().is_empty()) {
            lines.next();
        }
        let is_triplet = lines
            .peek()
            .is_some_and(|line| line.split_whitespace().count() == 3);

        let costs = if is_triplet {
            // MeCab format: "right_id left_id cost" per line
            // Store as left_id * N + right_id to match cost() lookup
            let mut costs = vec![0i16; expected];
            for line in lines {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() != 3 {
                    return Err(DictError::Parse(format!(
                        "expected 3 fields, got {}",
                        fields.len()
                    )));
                }
                let right_id: usize = fields[0]
                    .parse()
                    .map_err(|e| DictError::Parse(format!("right_id: {e}")))?;
                let left_id: usize = fields[1]
                    .parse()
                    .map_err(|e| DictError::Parse(format!("left_id: {e}")))?;
                let cost: i16 = fields[2]
                    .parse()
                    .map_err(|e| DictError::Parse(format!("cost: {e}")))?;
                let idx = left_id * num_ids as usize + right_id;
                if idx >= expected {
                    return Err(DictError::Parse(format!(
                        "index out of bounds: ({right_id}, {left_id})"
                    )));
                }
                costs[idx] = cost;
            }
            costs
        } else {
            // Mozc format: one cost per line
            let mut costs = Vec::with_capacity(expected);
            for line in lines {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let cost: i16 = line
                    .parse()
                    .map_err(|e| DictError::Parse(format!("invalid cost '{line}': {e}")))?;
                costs.push(cost);
            }
            if costs.len() != expected {
                return Err(DictError::Parse(format!(
                    "expected {expected} costs, got {}",
                    costs.len()
                )));
            }
            costs
        };

        Ok(Self {
            num_ids,
            fw_min: 0,
            fw_max: 0,
            roles: Vec::new(),
            header_size: V2_HEADER_SIZE,
            storage: CostStorage::Owned(costs),
        })
    }

    /// Build from a text file with function-word ID range metadata.
    pub fn from_text_with_metadata(
        text: &str,
        fw_min: u16,
        fw_max: u16,
    ) -> Result<Self, DictError> {
        let mut m = Self::from_text(text)?;
        m.fw_min = fw_min;
        m.fw_max = fw_max;
        Ok(m)
    }

    /// Build from a text file with function-word range and morpheme roles.
    pub fn from_text_with_roles(
        text: &str,
        fw_min: u16,
        fw_max: u16,
        roles: Vec<u8>,
    ) -> Result<Self, DictError> {
        let mut m = Self::from_text(text)?;
        m.fw_min = fw_min;
        m.fw_max = fw_max;
        m.roles = roles;
        Ok(m)
    }

    fn validate_header(data: &[u8]) -> Result<(u16, u16, u16, Vec<u8>, usize), DictError> {
        if data.len() < V1_HEADER_SIZE {
            return Err(DictError::InvalidHeader);
        }
        if &data[..4] != MAGIC {
            return Err(DictError::InvalidMagic);
        }
        let version = data[4];
        let (num_ids, fw_min, fw_max, roles, hdr_size) = match version {
            1 => {
                let num_ids = u16::from_le_bytes([data[5], data[6]]);
                (num_ids, 0u16, 0u16, Vec::new(), V1_HEADER_SIZE)
            }
            2 => {
                if data.len() < V2_HEADER_SIZE {
                    return Err(DictError::InvalidHeader);
                }
                let num_ids = u16::from_le_bytes([data[5], data[6]]);
                let fw_min = u16::from_le_bytes([data[7], data[8]]);
                let fw_max = u16::from_le_bytes([data[9], data[10]]);
                (num_ids, fw_min, fw_max, Vec::new(), V2_HEADER_SIZE)
            }
            3 => {
                if data.len() < V2_HEADER_SIZE {
                    return Err(DictError::InvalidHeader);
                }
                let num_ids = u16::from_le_bytes([data[5], data[6]]);
                let fw_min = u16::from_le_bytes([data[7], data[8]]);
                let fw_max = u16::from_le_bytes([data[9], data[10]]);
                let roles_end = V2_HEADER_SIZE + num_ids as usize;
                if data.len() < roles_end {
                    return Err(DictError::InvalidHeader);
                }
                let roles = data[V2_HEADER_SIZE..roles_end].to_vec();
                (num_ids, fw_min, fw_max, roles, roles_end)
            }
            _ => return Err(DictError::UnsupportedVersion(version)),
        };
        let expected_bytes = num_ids as usize * num_ids as usize * 2;
        let actual_bytes = data.len() - hdr_size;
        if actual_bytes != expected_bytes {
            return Err(DictError::Parse(format!(
                "expected {expected_bytes} bytes of cost data, got {actual_bytes}",
            )));
        }
        Ok((num_ids, fw_min, fw_max, roles, hdr_size))
    }

    /// Load from compiled binary format using memory-mapped I/O.
    ///
    /// The cost data is accessed directly from the mapped file, avoiding
    /// a heap allocation for the entire matrix. The OS pages in data on
    /// demand and can reclaim pages under memory pressure.
    pub fn open(path: &Path) -> Result<Self, DictError> {
        let file = File::open(path)?;
        // SAFETY: The file is opened read-only and the mapping is immutable.
        // We hold the Mmap for the lifetime of this struct, so the data remains
        // valid. The file should not be modified while the IME is running.
        let mmap = unsafe { Mmap::map(&file)? };
        let (num_ids, fw_min, fw_max, roles, hdr_size) = Self::validate_header(&mmap)?;
        Ok(Self {
            num_ids,
            fw_min,
            fw_max,
            roles,
            header_size: hdr_size,
            storage: CostStorage::Mapped(mmap),
        })
    }

    /// Parse from compiled binary format into an owned representation.
    pub fn from_bytes(data: &[u8]) -> Result<Self, DictError> {
        let (num_ids, fw_min, fw_max, roles, hdr_size) = Self::validate_header(data)?;
        let costs: Vec<i16> = data[hdr_size..]
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        Ok(Self {
            num_ids,
            fw_min,
            fw_max,
            roles,
            header_size: hdr_size,
            storage: CostStorage::Owned(costs),
        })
    }

    /// Serialize to compiled binary format (writes V3 if roles present, V2 otherwise).
    pub fn to_bytes(&self) -> Vec<u8> {
        let costs = match &self.storage {
            CostStorage::Owned(c) => c.as_slice(),
            CostStorage::Mapped(_) => {
                return self.to_bytes_from_mapped();
            }
        };
        let has_roles = !self.roles.is_empty();
        let version = if has_roles { 3u8 } else { 2u8 };
        let roles_size = if has_roles { self.roles.len() } else { 0 };
        let mut buf = Vec::with_capacity(V2_HEADER_SIZE + roles_size + costs.len() * 2);
        buf.extend_from_slice(MAGIC);
        buf.push(version);
        buf.extend_from_slice(&self.num_ids.to_le_bytes());
        buf.extend_from_slice(&self.fw_min.to_le_bytes());
        buf.extend_from_slice(&self.fw_max.to_le_bytes());
        if has_roles {
            buf.extend_from_slice(&self.roles);
        }
        for &cost in costs {
            buf.extend_from_slice(&cost.to_le_bytes());
        }
        buf
    }

    /// Helper: re-serialize a Mapped matrix.
    fn to_bytes_from_mapped(&self) -> Vec<u8> {
        let n = self.num_ids as usize * self.num_ids as usize;
        let has_roles = !self.roles.is_empty();
        let version = if has_roles { 3u8 } else { 2u8 };
        let roles_size = if has_roles { self.roles.len() } else { 0 };
        let mut buf = Vec::with_capacity(V2_HEADER_SIZE + roles_size + n * 2);
        buf.extend_from_slice(MAGIC);
        buf.push(version);
        buf.extend_from_slice(&self.num_ids.to_le_bytes());
        buf.extend_from_slice(&self.fw_min.to_le_bytes());
        buf.extend_from_slice(&self.fw_max.to_le_bytes());
        if has_roles {
            buf.extend_from_slice(&self.roles);
        }
        for i in 0..n {
            let left = (i / self.num_ids as usize) as u16;
            let right = (i % self.num_ids as usize) as u16;
            buf.extend_from_slice(&self.cost(left, right).to_le_bytes());
        }
        buf
    }

    /// Save compiled binary to file.
    pub fn save(&self, path: &Path) -> Result<(), DictError> {
        Ok(fs::write(path, self.to_bytes())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_matrix() -> ConnectionMatrix {
        let text = "3 3\n0\n10\n20\n30\n40\n50\n60\n70\n80\n";
        ConnectionMatrix::from_text(text).unwrap()
    }

    #[test]
    fn test_from_text() {
        let m = sample_matrix();
        assert_eq!(m.num_ids(), 3);
        // Row 0: [0, 10, 20]
        assert_eq!(m.cost(0, 0), 0);
        assert_eq!(m.cost(0, 1), 10);
        assert_eq!(m.cost(0, 2), 20);
        // Row 1: [30, 40, 50]
        assert_eq!(m.cost(1, 0), 30);
        assert_eq!(m.cost(1, 1), 40);
        assert_eq!(m.cost(1, 2), 50);
        // Row 2: [60, 70, 80]
        assert_eq!(m.cost(2, 0), 60);
        assert_eq!(m.cost(2, 1), 70);
        assert_eq!(m.cost(2, 2), 80);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let m = sample_matrix();
        let bytes = m.to_bytes();
        let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
        assert_eq!(m2.num_ids(), m.num_ids());
        for left in 0..m.num_ids() {
            for right in 0..m.num_ids() {
                assert_eq!(m.cost(left, right), m2.cost(left, right));
            }
        }
    }

    #[test]
    fn test_file_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_conn");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.conn");

        let m = sample_matrix();
        m.save(&path).unwrap();

        let m2 = ConnectionMatrix::open(&path).unwrap();
        assert_eq!(m2.num_ids(), 3);
        assert_eq!(m.cost(1, 2), m2.cost(1, 2));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_invalid_magic() {
        let result = ConnectionMatrix::from_bytes(b"XXXX\x01\x03\x00");
        assert!(matches!(result, Err(DictError::InvalidMagic)));
    }

    #[test]
    fn test_header_too_short() {
        let result = ConnectionMatrix::from_bytes(b"LXC");
        assert!(matches!(result, Err(DictError::InvalidHeader)));
    }

    #[test]
    fn test_unsupported_version() {
        let result = ConnectionMatrix::from_bytes(b"LXCX\x99\x01\x00");
        assert!(matches!(result, Err(DictError::UnsupportedVersion(0x99))));
    }

    #[test]
    fn test_negative_costs() {
        let text = "2 2\n-100\n200\n-300\n400\n";
        let m = ConnectionMatrix::from_text(text).unwrap();
        assert_eq!(m.cost(0, 0), -100);
        assert_eq!(m.cost(0, 1), 200);
        assert_eq!(m.cost(1, 0), -300);
        assert_eq!(m.cost(1, 1), 400);
    }

    #[test]
    fn test_wrong_count() {
        let text = "2 2\n0\n10\n20\n"; // only 3 costs instead of 4
        let result = ConnectionMatrix::from_text(text);
        assert!(matches!(result, Err(DictError::Parse(_))));
    }

    #[test]
    fn test_mecab_triplet_format() {
        // MeCab matrix.def: "right_id left_id cost"
        // Line "R L C" → cost(left=L, right=R) = C
        // Use asymmetric values to catch transpose bugs
        let text = "2 2\n0 0 10\n0 1 20\n1 0 30\n1 1 40\n";
        let m = ConnectionMatrix::from_text(text).unwrap();
        assert_eq!(m.num_ids(), 2);
        // "0 0 10" → cost(left=0, right=0) = 10
        assert_eq!(m.cost(0, 0), 10);
        // "0 1 20" → cost(left=1, right=0) = 20
        assert_eq!(m.cost(1, 0), 20);
        // "1 0 30" → cost(left=0, right=1) = 30
        assert_eq!(m.cost(0, 1), 30);
        // "1 1 40" → cost(left=1, right=1) = 40
        assert_eq!(m.cost(1, 1), 40);
    }

    #[test]
    fn test_mecab_triplet_sparse() {
        // Sparse: only specify some entries; rest default to 0
        // Format: "right_id left_id cost"
        // "0 1 100" → right=0, left=1 → cost(left=1, right=0) = 100
        // "1 0 -200" → right=1, left=0 → cost(left=0, right=1) = -200
        let text = "2 2\n0 1 100\n1 0 -200\n";
        let m = ConnectionMatrix::from_text(text).unwrap();
        assert_eq!(m.cost(0, 0), 0);
        assert_eq!(m.cost(0, 1), -200);
        assert_eq!(m.cost(1, 0), 100);
        assert_eq!(m.cost(1, 1), 0);
    }

    #[test]
    fn test_mecab_triplet_roundtrip() {
        let text = "2 2\n0 0 10\n0 1 20\n1 0 30\n1 1 40\n";
        let m = ConnectionMatrix::from_text(text).unwrap();
        let bytes = m.to_bytes();
        let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
        assert_eq!(m2.num_ids(), 2);
        for left in 0..2 {
            for right in 0..2 {
                assert_eq!(m.cost(left, right), m2.cost(left, right));
            }
        }
    }

    #[test]
    fn test_v2_roundtrip_with_metadata() {
        let text = "3 3\n0\n10\n20\n30\n40\n50\n60\n70\n80\n";
        let m = ConnectionMatrix::from_text_with_metadata(text, 29, 433).unwrap();
        assert!(m.is_function_word(29));
        assert!(m.is_function_word(200));
        assert!(m.is_function_word(433));
        assert!(!m.is_function_word(28));
        assert!(!m.is_function_word(434));

        let bytes = m.to_bytes();
        let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
        assert_eq!(m2.num_ids(), 3);
        assert!(m2.is_function_word(100));
        assert!(!m2.is_function_word(0));
        for left in 0..3 {
            for right in 0..3 {
                assert_eq!(m.cost(left, right), m2.cost(left, right));
            }
        }
    }

    #[test]
    fn test_is_function_word_no_range() {
        let m = sample_matrix();
        // fw_min == 0 && fw_max == 0 → always false
        assert!(!m.is_function_word(0));
        assert!(!m.is_function_word(100));
    }

    #[test]
    fn test_v2_file_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_conn_v2");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_v2.conn");

        let text = "2 2\n10\n20\n30\n40\n";
        let m = ConnectionMatrix::from_text_with_metadata(text, 50, 300).unwrap();
        m.save(&path).unwrap();

        let m2 = ConnectionMatrix::open(&path).unwrap();
        assert_eq!(m2.num_ids(), 2);
        assert!(m2.is_function_word(100));
        assert!(!m2.is_function_word(49));
        assert_eq!(m2.cost(0, 0), 10);
        assert_eq!(m2.cost(1, 1), 40);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_v1_backward_compat() {
        // Construct a V1 binary manually
        let mut v1_bytes = Vec::new();
        v1_bytes.extend_from_slice(MAGIC);
        v1_bytes.push(1); // V1
        v1_bytes.extend_from_slice(&2u16.to_le_bytes()); // num_ids = 2
                                                         // 4 costs: 10, 20, 30, 40
        for cost in [10i16, 20, 30, 40] {
            v1_bytes.extend_from_slice(&cost.to_le_bytes());
        }

        let m = ConnectionMatrix::from_bytes(&v1_bytes).unwrap();
        assert_eq!(m.num_ids(), 2);
        assert_eq!(m.cost(0, 0), 10);
        assert_eq!(m.cost(0, 1), 20);
        assert_eq!(m.cost(1, 0), 30);
        assert_eq!(m.cost(1, 1), 40);
        // V1 has no function-word range or roles
        assert!(!m.is_function_word(100));
        assert_eq!(m.role(0), 0);
        assert!(!m.is_suffix(0));
        assert!(!m.is_prefix(0));
    }

    #[test]
    fn test_v3_roundtrip_with_roles() {
        // 4 IDs: 0=content, 1=function, 2=suffix, 3=prefix
        let text = "4 4\n";
        let costs_text: String = (0..16).map(|i| format!("{}\n", i * 10)).collect::<String>();
        let full_text = format!("{text}{costs_text}");
        let roles = vec![0, 1, 2, 3];
        let m = ConnectionMatrix::from_text_with_roles(&full_text, 1, 1, roles.clone()).unwrap();

        assert_eq!(m.role(0), 0);
        assert_eq!(m.role(1), 1);
        assert_eq!(m.role(2), 2);
        assert_eq!(m.role(3), 3);
        assert!(!m.is_prefix(0));
        assert!(!m.is_suffix(0));
        assert!(m.is_suffix(2));
        assert!(m.is_prefix(3));
        assert!(m.is_function_word(1));

        // Serialize and deserialize
        let bytes = m.to_bytes();
        // Check V3 marker
        assert_eq!(bytes[4], 3);

        let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
        assert_eq!(m2.num_ids(), 4);
        assert_eq!(m2.role(0), 0);
        assert_eq!(m2.role(1), 1);
        assert_eq!(m2.role(2), 2);
        assert_eq!(m2.role(3), 3);
        assert!(m2.is_suffix(2));
        assert!(m2.is_prefix(3));
        for left in 0..4 {
            for right in 0..4 {
                assert_eq!(m.cost(left, right), m2.cost(left, right));
            }
        }
    }

    #[test]
    fn test_v3_file_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_conn_v3");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_v3.conn");

        let text = "3 3\n0\n10\n20\n30\n40\n50\n60\n70\n80\n";
        let roles = vec![0, 2, 3]; // content, suffix, prefix
        let m = ConnectionMatrix::from_text_with_roles(text, 0, 0, roles).unwrap();
        m.save(&path).unwrap();

        let m2 = ConnectionMatrix::open(&path).unwrap();
        assert_eq!(m2.num_ids(), 3);
        assert_eq!(m2.role(0), 0);
        assert_eq!(m2.role(1), 2);
        assert_eq!(m2.role(2), 3);
        assert!(m2.is_suffix(1));
        assert!(m2.is_prefix(2));
        assert_eq!(m2.cost(0, 1), 10);
        assert_eq!(m2.cost(2, 2), 80);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_v2_backward_compat_no_roles() {
        // V2 binary should load with empty roles → role() always returns 0
        let text = "2 2\n10\n20\n30\n40\n";
        let m = ConnectionMatrix::from_text_with_metadata(text, 50, 300).unwrap();
        let bytes = m.to_bytes();
        // Should write V2 (no roles)
        assert_eq!(bytes[4], 2);

        let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
        assert_eq!(m2.role(0), 0);
        assert_eq!(m2.role(1), 0);
        assert!(!m2.is_suffix(0));
        assert!(!m2.is_prefix(0));
    }
}
