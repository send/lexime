//! Orchestration for `dictool candidates` subcommands.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use lex_core::dict::{Dictionary, TrieDictionary};

use crate::candidates::sudachi;
use crate::candidates::{classify, write_candidates, Bucket, Candidate, CandidateError};

/// Mine extras candidates from SudachiDict.
///
/// Steps:
/// 1. Fetch SudachiDict-full into `cache_dir` (idempotent).
/// 2. Parse all `*_lex.csv`, returning `(reading, surface, cost, pos)` rows.
/// 3. Diff against the merged build dict at `build_dict_path`. Drop rows where
///    `(reading, surface)` is already represented.
/// 4. Classify each remaining row into a bucket (place / common / other).
/// 5. Write per-bucket TSVs into `out_dir`.
pub fn mine(
    cache_dir: &Path,
    build_dict_path: &Path,
    out_dir: &Path,
) -> Result<(), CandidateError> {
    let version = sudachi::fetch(cache_dir)?;
    let upstream = sudachi::parse_dir(cache_dir)?;

    let dict = TrieDictionary::open(build_dict_path).map_err(|e| {
        CandidateError::Parse(format!(
            "open build dict {}: {e}",
            build_dict_path.display()
        ))
    })?;

    let mut candidates: Vec<(Bucket, Candidate)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut total_upstream = 0usize;
    let mut already_covered = 0usize;

    for (reading, cands) in &upstream {
        // For a typical reading the build dict yields a handful of entries
        // (homophones), so a linear scan is faster than building a HashSet
        // and avoids cloning every surface up front.
        let existing = dict.lookup(reading);
        for cand in cands {
            total_upstream += 1;
            if existing.iter().any(|e| e.surface == cand.surface) {
                already_covered += 1;
                continue;
            }
            // Dedupe in case Sudachi has multiple POS variants for the same
            // (reading, surface) — we don't care which POS won here.
            if !seen.insert((cand.reading.clone(), cand.surface.clone())) {
                continue;
            }
            let pos_fields: Vec<&str> = cand.pos.split('-').collect();
            let bucket = classify(&pos_fields);
            candidates.push((bucket, cand.clone()));
        }
    }

    eprintln!(
        "Mined {} candidates ({} upstream, {} already in build dict)",
        candidates.len(),
        total_upstream,
        already_covered
    );

    write_candidates(out_dir, &version, &candidates)?;
    Ok(())
}

/// Default cache dir for the working SudachiDict download. Sits under
/// `engine/data/` like the other dict artifacts, but with a leading dot so
/// it sorts away from the production caches (`mozc-raw/`, `extras-raw/`)
/// and signals "internal scratch — `dictool candidates mine` owns it".
/// Gitignored alongside the rest of `engine/data/`.
pub fn default_cache_dir() -> PathBuf {
    PathBuf::from("engine/data/.sudachi-cache")
}

/// Default output dir for mined candidate TSVs. Gitignored.
pub fn default_out_dir() -> PathBuf {
    PathBuf::from("engine/data/extras-candidates")
}

/// Default build dict path. The mined candidates are diffed against this.
pub fn default_build_dict() -> PathBuf {
    PathBuf::from("engine/data/lexime.dict")
}

/// Convenience wrapper for fresh-state runs that wipes the previous output
/// before re-mining (so removed Sudachi entries don't linger).
pub fn clean_out_dir(out_dir: &Path) -> Result<(), CandidateError> {
    if out_dir.exists() {
        fs::remove_dir_all(out_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_are_under_engine_data() {
        assert!(default_cache_dir().starts_with("engine/data"));
        assert!(default_out_dir().starts_with("engine/data"));
        assert!(default_build_dict().starts_with("engine/data"));
    }

    #[test]
    fn clean_out_dir_is_idempotent() {
        let p = std::env::temp_dir().join("lexime_test_clean_out_dir");
        let _ = fs::remove_dir_all(&p);

        // Should succeed even when the dir doesn't exist.
        clean_out_dir(&p).unwrap();

        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("dummy"), "").unwrap();
        clean_out_dir(&p).unwrap();
        assert!(!p.exists());
    }
}
