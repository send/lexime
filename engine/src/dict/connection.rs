use std::fs;
use std::io;
use std::path::Path;

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

    /// Build from a TSV file (Mozc connection_single_column.txt format).
    ///
    /// Format:
    /// - Line 1: `num_left num_right` (both should be equal)
    /// - Remaining lines: one cost (i16) per line, row-major order
    pub fn from_text(text: &str) -> Result<Self, ConnectionError> {
        let mut lines = text.lines();

        let header = lines
            .next()
            .ok_or_else(|| ConnectionError::Parse("empty file".to_string()))?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        let num_left: u16 = match parts.len() {
            1 => parts[0]
                .parse()
                .map_err(|e| ConnectionError::Parse(format!("invalid num_ids: {e}")))?,
            2 => {
                let nl: u16 = parts[0]
                    .parse()
                    .map_err(|e| ConnectionError::Parse(format!("invalid num_left: {e}")))?;
                let nr: u16 = parts[1]
                    .parse()
                    .map_err(|e| ConnectionError::Parse(format!("invalid num_right: {e}")))?;
                if nl != nr {
                    return Err(ConnectionError::Parse(format!(
                        "num_left ({nl}) != num_right ({nr})"
                    )));
                }
                nl
            }
            _ => {
                return Err(ConnectionError::Parse(format!(
                    "expected 1 or 2 values in header, got {}",
                    parts.len()
                )));
            }
        };

        let expected = num_left as usize * num_left as usize;
        let mut costs = Vec::with_capacity(expected);

        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let cost: i16 = line
                .parse()
                .map_err(|e| ConnectionError::Parse(format!("invalid cost '{line}': {e}")))?;
            costs.push(cost);
        }

        if costs.len() != expected {
            return Err(ConnectionError::Parse(format!(
                "expected {expected} costs, got {}",
                costs.len()
            )));
        }

        Ok(Self {
            num_ids: num_left,
            costs,
        })
    }

    /// Load from compiled binary format.
    pub fn open(path: &Path) -> Result<Self, ConnectionError> {
        let data = fs::read(path).map_err(ConnectionError::Io)?;
        Self::from_bytes(&data)
    }

    /// Parse from compiled binary format.
    pub fn from_bytes(data: &[u8]) -> Result<Self, ConnectionError> {
        if data.len() < HEADER_SIZE {
            return Err(ConnectionError::InvalidHeader);
        }
        if &data[..4] != MAGIC {
            return Err(ConnectionError::InvalidMagic);
        }
        if data[4] != VERSION {
            return Err(ConnectionError::UnsupportedVersion(data[4]));
        }

        let num_ids = u16::from_le_bytes([data[5], data[6]]);
        let expected_len = num_ids as usize * num_ids as usize;
        let costs_bytes = &data[HEADER_SIZE..];

        if costs_bytes.len() != expected_len * 2 {
            return Err(ConnectionError::Parse(format!(
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
    pub fn save(&self, path: &Path) -> Result<(), ConnectionError> {
        fs::write(path, self.to_bytes()).map_err(ConnectionError::Io)
    }
}

#[derive(Debug)]
pub enum ConnectionError {
    Io(io::Error),
    InvalidHeader,
    InvalidMagic,
    UnsupportedVersion(u8),
    Parse(String),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::InvalidHeader => write!(f, "invalid connection matrix header"),
            Self::InvalidMagic => write!(f, "invalid magic bytes (expected LXCX)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported version: {v}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
        }
    }
}

impl std::error::Error for ConnectionError {}

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
        assert!(matches!(result, Err(ConnectionError::InvalidMagic)));
    }

    #[test]
    fn test_header_too_short() {
        let result = ConnectionMatrix::from_bytes(b"LXC");
        assert!(matches!(result, Err(ConnectionError::InvalidHeader)));
    }

    #[test]
    fn test_unsupported_version() {
        let result = ConnectionMatrix::from_bytes(b"LXCX\x99\x01\x00");
        assert!(matches!(
            result,
            Err(ConnectionError::UnsupportedVersion(0x99))
        ));
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
        assert!(matches!(result, Err(ConnectionError::Parse(_))));
    }
}
