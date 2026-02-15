use std::fs::{self, File};
use std::path::Path;

use memmap2::Mmap;

use super::connection::{ConnectionMatrix, CostStorage, MAGIC, V1_HEADER_SIZE, V2_HEADER_SIZE};
use super::DictError;

impl ConnectionMatrix {
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

        Ok(Self::new_owned(num_ids, 0, 0, Vec::new(), costs))
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
    ///
    /// `roles` must have length ≤ `num_ids`. IDs beyond the roles vector
    /// length are treated as content words (role 0) by `role()`.
    pub fn from_text_with_roles(
        text: &str,
        fw_min: u16,
        fw_max: u16,
        roles: Vec<u8>,
    ) -> Result<Self, DictError> {
        let mut m = Self::from_text(text)?;
        if roles.len() > m.num_ids as usize {
            return Err(DictError::InvalidHeader);
        }
        m.fw_min = fw_min;
        m.fw_max = fw_max;
        m.roles = roles;
        Ok(m)
    }

    pub(super) fn validate_header(
        data: &[u8],
    ) -> Result<(u16, u16, u16, Vec<u8>, usize), DictError> {
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
        Ok(Self::new_owned(num_ids, fw_min, fw_max, roles, costs))
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
