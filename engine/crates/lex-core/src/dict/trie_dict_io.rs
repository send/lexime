use std::fs::{self, File};
use std::path::Path;

use lexime_trie::{DoubleArray, DoubleArrayRef};
use memmap2::Mmap;

use super::trie_dict::{
    TrieDictionary, TrieStore, ValuesStore, HEADER_SIZE, MAGIC, SLOT_SIZE, VERSION,
};
use super::DictError;

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
        let reading_count: u32 = (index.len() / SLOT_SIZE) as u32;

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

        let trie_len = u32::from_ne_bytes(data[8..12].try_into().unwrap()) as usize;
        let pool_len = u32::from_ne_bytes(data[12..16].try_into().unwrap()) as usize;
        let entries_len = u32::from_ne_bytes(data[16..20].try_into().unwrap()) as usize;
        let reading_count = u32::from_ne_bytes(data[20..24].try_into().unwrap()) as usize;
        let index_len = reading_count * SLOT_SIZE;

        let expected = HEADER_SIZE + trie_len + pool_len + entries_len + index_len;
        if data.len() < expected {
            return Err(DictError::InvalidHeader);
        }

        let trie_start = HEADER_SIZE;
        let pool_start = trie_start + trie_len;
        let entries_start = pool_start + pool_len;
        let index_start = entries_start + entries_len;

        let trie = DoubleArray::<u8>::from_bytes(&data[trie_start..trie_start + trie_len])?;

        Ok(Self {
            trie: TrieStore::Owned(trie),
            values: ValuesStore::Owned {
                string_pool: data[pool_start..pool_start + pool_len].to_vec(),
                entries_data: data[entries_start..entries_start + entries_len].to_vec(),
                reading_index: data[index_start..index_start + index_len].to_vec(),
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
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < 5 {
            return Err(DictError::InvalidHeader);
        }
        if &mmap[..4] != MAGIC {
            return Err(DictError::InvalidMagic);
        }
        if mmap[4] != VERSION {
            return Err(DictError::UnsupportedVersion(mmap[4]));
        }
        if mmap.len() < HEADER_SIZE {
            return Err(DictError::InvalidHeader);
        }

        let trie_len = u32::from_ne_bytes(mmap[8..12].try_into().unwrap()) as usize;
        let pool_len = u32::from_ne_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let entries_len = u32::from_ne_bytes(mmap[16..20].try_into().unwrap()) as usize;
        let reading_count = u32::from_ne_bytes(mmap[20..24].try_into().unwrap()) as usize;
        let index_len = reading_count * SLOT_SIZE;

        let expected = HEADER_SIZE + trie_len + pool_len + entries_len + index_len;
        if mmap.len() < expected {
            return Err(DictError::InvalidHeader);
        }

        let trie_start = HEADER_SIZE;
        let pool_start = trie_start + trie_len;
        let entries_start = pool_start + pool_len;
        let index_start = entries_start + entries_len;

        // Zero-copy trie from mmap
        let trie_ref =
            DoubleArrayRef::<u8>::from_bytes_ref(&mmap[trie_start..trie_start + trie_len])?;
        // SAFETY: The mmap is stored in self._mmap and will be dropped after trie and values
        // (Rust drops fields in declaration order: trie, values, _mmap).
        let trie_ref = unsafe {
            std::mem::transmute::<DoubleArrayRef<'_, u8>, DoubleArrayRef<'static, u8>>(trie_ref)
        };

        // SAFETY: The slices reference mmap data. The mmap is stored in self._mmap
        // and will outlive these references (dropped last due to field order).
        let string_pool = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(&mmap[pool_start..pool_start + pool_len])
        };
        let entries_data = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(
                &mmap[entries_start..entries_start + entries_len],
            )
        };
        let reading_index = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(&mmap[index_start..index_start + index_len])
        };

        Ok(Self {
            trie: TrieStore::MmapRef(trie_ref),
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
