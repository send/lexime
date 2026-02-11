use std::fs;
use std::path::Path;

use super::DictError;

const MAGIC: &[u8; 4] = b"LXCX";
const VERSION: u8 = 1;
const HEADER_SIZE: usize = 4 + 1 + 2; // magic + version + num_ids

/// A connection cost matrix mapping (left_id, right_id) → cost.
/// Used by the Viterbi algorithm to score morpheme transitions.
pub struct ConnectionMatrix {
    num_ids: u16,
    costs: Vec<i16>,
}

impl ConnectionMatrix {
    /// Look up the connection cost between two morphemes.
    /// Index: left_id * num_ids + right_id. Max index with u16 is ~4.3B,
    /// which fits in usize on 64-bit targets. Out-of-bounds returns 0.
    pub fn cost(&self, left_id: u16, right_id: u16) -> i16 {
        let idx = (left_id as usize)
            .saturating_mul(self.num_ids as usize)
            .saturating_add(right_id as usize);
        self.costs.get(idx).copied().unwrap_or(0)
    }

    /// Number of morpheme IDs in this matrix.
    pub fn num_ids(&self) -> u16 {
        self.num_ids
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

        Ok(Self { num_ids, costs })
    }

    /// Load from compiled binary format.
    pub fn open(path: &Path) -> Result<Self, DictError> {
        let data = fs::read(path)?;
        Self::from_bytes(&data)
    }

    /// Parse from compiled binary format.
    pub fn from_bytes(data: &[u8]) -> Result<Self, DictError> {
        if data.len() < HEADER_SIZE {
            return Err(DictError::InvalidHeader);
        }
        if &data[..4] != MAGIC {
            return Err(DictError::InvalidMagic);
        }
        if data[4] != VERSION {
            return Err(DictError::UnsupportedVersion(data[4]));
        }

        let num_ids = u16::from_le_bytes([data[5], data[6]]);
        let expected_len = num_ids as usize * num_ids as usize;
        let costs_bytes = &data[HEADER_SIZE..];

        if costs_bytes.len() != expected_len * 2 {
            return Err(DictError::Parse(format!(
                "expected {} bytes of cost data, got {}",
                expected_len * 2,
                costs_bytes.len()
            )));
        }

        let costs: Vec<i16> = costs_bytes
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        Ok(Self { num_ids, costs })
    }

    /// Serialize to compiled binary format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.costs.len() * 2);
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&self.num_ids.to_le_bytes());
        for &cost in &self.costs {
            buf.extend_from_slice(&cost.to_le_bytes());
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
}
