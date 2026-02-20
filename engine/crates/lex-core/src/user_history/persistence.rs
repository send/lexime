use std::fs;
use std::io;
use std::path::Path;

use super::{
    BigramRecord, HistoryEntry, UnigramRecord, UserHistory, UserHistoryData, MAGIC, VERSION,
};

impl UserHistory {
    /// Serialize to bytes (LXUD format).
    pub fn to_bytes(&self) -> Result<Vec<u8>, io::Error> {
        let data = self.to_data();
        let body = bincode::serialize(&data).map_err(io::Error::other)?;

        let mut buf = Vec::with_capacity(5 + body.len());
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&body);
        Ok(buf)
    }

    /// Deserialize from bytes (LXUD format).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        if bytes.len() < 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too short"));
        }
        if &bytes[0..4] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad magic"));
        }
        if bytes[4] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported version",
            ));
        }
        let data: UserHistoryData = bincode::deserialize(&bytes[5..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Self::from_data(data))
    }

    /// Atomic write: write to .tmp then rename.
    pub fn save(&self, path: &Path) -> Result<(), io::Error> {
        let bytes = self.to_bytes()?;
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Open from file, returning empty UserHistory if file doesn't exist.
    pub fn open(path: &Path) -> Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }

    pub(super) fn to_data(&self) -> UserHistoryData {
        let mut unigrams = Vec::new();
        for (reading, inner) in &self.unigrams {
            for (surface, entry) in inner {
                unigrams.push(UnigramRecord {
                    reading: reading.clone(),
                    surface: surface.clone(),
                    frequency: entry.frequency,
                    last_used: entry.last_used,
                });
            }
        }

        let mut bigrams = Vec::new();
        for (prev, inner) in &self.bigrams {
            for ((next_r, next_s), entry) in inner {
                bigrams.push(BigramRecord {
                    prev_surface: prev.clone(),
                    next_reading: next_r.clone(),
                    next_surface: next_s.clone(),
                    frequency: entry.frequency,
                    last_used: entry.last_used,
                });
            }
        }

        UserHistoryData { unigrams, bigrams }
    }

    pub(super) fn from_data(data: UserHistoryData) -> Self {
        let mut unigrams: std::collections::HashMap<
            String,
            std::collections::HashMap<String, HistoryEntry>,
        > = std::collections::HashMap::new();
        for rec in data.unigrams {
            unigrams.entry(rec.reading).or_default().insert(
                rec.surface,
                HistoryEntry {
                    frequency: rec.frequency,
                    last_used: rec.last_used,
                },
            );
        }

        let mut bigrams: std::collections::HashMap<
            String,
            std::collections::HashMap<(String, String), HistoryEntry>,
        > = std::collections::HashMap::new();
        for rec in data.bigrams {
            bigrams.entry(rec.prev_surface).or_default().insert(
                (rec.next_reading, rec.next_surface),
                HistoryEntry {
                    frequency: rec.frequency,
                    last_used: rec.last_used,
                },
            );
        }

        Self { unigrams, bigrams }
    }
}
