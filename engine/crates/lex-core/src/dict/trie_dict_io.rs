use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use lexime_trie::{DoubleArray, DoubleArrayBacked};
use memmap2::Mmap;

use super::trie_dict::{
    OwnedMmap, TrieDictionary, TrieStore, ValuesStore, HEADER_SIZE, MAGIC, SLOT_SIZE, VERSION,
};
use super::DictError;

/// Validated byte offsets for each LXDX section.
///
/// Produced by [`SectionOffsets::parse`], which also performs the
/// magic / version / field reads so the two call sites (`open` and
/// `from_bytes`) share a single source of truth for the on-disk
/// layout. Once constructed, the offsets are known-good and
/// downstream slicing is panic-free.
struct SectionOffsets {
    trie_start: usize,
    pool_start: usize,
    entries_start: usize,
    index_start: usize,
    end: usize,
}

impl SectionOffsets {
    /// Validate the LXDX header in `data` and compute section offsets.
    fn parse(data: &[u8]) -> Result<Self, DictError> {
        if data.len() < 5 {
            return Err(DictError::InvalidHeader);
        }
        if &data[..4] != MAGIC {
            return Err(DictError::InvalidMagic);
        }
        if data[4] != VERSION {
            return Err(DictError::UnsupportedVersion(data[4]));
        }
        if data.len() < HEADER_SIZE {
            return Err(DictError::InvalidHeader);
        }

        let read_u32 = |range: std::ops::Range<usize>| {
            u32::from_ne_bytes(data[range].try_into().expect("4-byte header field")) as usize
        };
        let trie_len = read_u32(8..12);
        let pool_len = read_u32(12..16);
        let entries_len = read_u32(16..20);
        let reading_count = read_u32(20..24);

        let index_len = reading_count
            .checked_mul(SLOT_SIZE)
            .ok_or(DictError::InvalidHeader)?;
        let trie_start = HEADER_SIZE;
        let pool_start = trie_start
            .checked_add(trie_len)
            .ok_or(DictError::InvalidHeader)?;
        let entries_start = pool_start
            .checked_add(pool_len)
            .ok_or(DictError::InvalidHeader)?;
        let index_start = entries_start
            .checked_add(entries_len)
            .ok_or(DictError::InvalidHeader)?;
        let end = index_start
            .checked_add(index_len)
            .ok_or(DictError::InvalidHeader)?;
        if data.len() < end {
            return Err(DictError::InvalidHeader);
        }
        Ok(Self {
            trie_start,
            pool_start,
            entries_start,
            index_start,
            end,
        })
    }
}

impl TrieDictionary {
    pub fn to_bytes(&self) -> Result<Vec<u8>, DictError> {
        let trie_data = match &self.trie {
            TrieStore::Owned(da) => da.as_bytes(),
            TrieStore::MmapRef(_) => {
                return Err(DictError::Parse(
                    "cannot serialize mmap-backed dictionary".into(),
                ));
            }
        };

        let pool = self.values.string_pool();
        let entries = self.values.entries_data();
        let index = self.values.reading_index();

        let trie_len: u32 = trie_data
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("trie data exceeds u32::MAX".to_string()))?;
        let pool_len: u32 = pool
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("string pool exceeds u32::MAX".to_string()))?;
        let entries_len: u32 = entries
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("entries data exceeds u32::MAX".to_string()))?;
        let reading_count: u32 = (index.len() / SLOT_SIZE)
            .try_into()
            .map_err(|_| DictError::Parse("reading count exceeds u32::MAX".to_string()))?;

        let total = HEADER_SIZE + trie_data.len() + pool.len() + entries.len() + index.len();
        let mut buf = Vec::with_capacity(total);
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 3]); // reserved
        buf.extend_from_slice(&trie_len.to_ne_bytes());
        buf.extend_from_slice(&pool_len.to_ne_bytes());
        buf.extend_from_slice(&entries_len.to_ne_bytes());
        buf.extend_from_slice(&reading_count.to_ne_bytes());
        buf.extend_from_slice(&trie_data);
        buf.extend_from_slice(pool);
        buf.extend_from_slice(entries);
        buf.extend_from_slice(index);

        Ok(buf)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, DictError> {
        let sections = SectionOffsets::parse(data)?;
        let trie = DoubleArray::<u8>::from_bytes(&data[sections.trie_start..sections.pool_start])?;

        Ok(Self {
            trie: TrieStore::Owned(trie),
            values: ValuesStore::Owned {
                string_pool: data[sections.pool_start..sections.entries_start].to_vec(),
                entries_data: data[sections.entries_start..sections.index_start].to_vec(),
                reading_index: data[sections.index_start..sections.end].to_vec(),
            },
            _mmap: None,
        })
    }

    /// Open a dictionary file, using mmap for zero-copy access.
    ///
    /// Both the trie and values data are referenced directly from the
    /// memory-mapped region, eliminating ~60-80MB of heap allocation.
    pub fn open(path: &Path) -> Result<Self, DictError> {
        let file = File::open(path)?;
        // SAFETY: The file is opened read-only and the mapping is immutable.
        let mmap = Arc::new(unsafe { Mmap::map(&file)? });
        let sections = SectionOffsets::parse(&mmap)?;
        let trie_len = sections.pool_start - sections.trie_start;

        // The trie backing owns its own `Arc<Mmap>` clone so the mapping
        // cannot be released out from under it even if the surrounding
        // `_mmap` slot is dropped first.
        let backed = DoubleArrayBacked::<u8, OwnedMmap>::from_backing(OwnedMmap::new(
            mmap.clone(),
            sections.trie_start,
            trie_len,
        )?)?;

        // SAFETY: `DoubleArrayBacked` only owns the trie slice; the
        // string pool / entry records / reading index are plain byte
        // slices so we keep the self-referential transmute for them.
        // `_mmap` stores a clone of the `Arc<Mmap>` and field drop
        // order (trie → values → `_mmap`) guarantees the mapping
        // outlives every borrow.
        let string_pool = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(
                &mmap[sections.pool_start..sections.entries_start],
            )
        };
        let entries_data = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(
                &mmap[sections.entries_start..sections.index_start],
            )
        };
        let reading_index = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(&mmap[sections.index_start..sections.end])
        };

        Ok(Self {
            trie: TrieStore::MmapRef(backed),
            values: ValuesStore::MmapRef {
                string_pool,
                entries_data,
                reading_index,
            },
            _mmap: Some(mmap),
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), DictError> {
        Ok(fs::write(path, self.to_bytes()?)?)
    }
}
