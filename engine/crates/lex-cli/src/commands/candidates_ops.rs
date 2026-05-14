//! Orchestration for `dictool candidates` subcommands.

use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use lex_core::dict::{Dictionary, TrieDictionary};

use crate::candidates::sudachi;
use crate::candidates::wikipedia;
use crate::candidates::{classify_pos_string, write_candidates, Bucket, Candidate, CandidateError};

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
    let mut total_upstream = 0usize;
    let mut already_covered = 0usize;

    // Consume upstream by value so each row's `surface` / `pos` String can
    // be moved into Candidate instead of cloned. Reading is still cloned
    // once per row (the HashMap key remains the canonical owner across the
    // outer loop), but per-row body strings move.
    for (reading, rows) in upstream {
        // For a typical reading the build dict yields a handful of entries
        // (homophones), so a linear scan is faster than building a HashSet
        // and avoids cloning every surface up front.
        let existing = dict.lookup(&reading);
        // Dedupe per reading: surfaces are only repeated when Sudachi has
        // several POS variants for the same (reading, surface), which is a
        // local property. Scoping `seen` to one reading drops the global
        // (reading, surface) HashSet that previously grew to O(rows) and
        // held reading clones across the entire mine.
        let mut seen: HashSet<String> = HashSet::new();
        for row in rows {
            total_upstream += 1;
            if existing.iter().any(|e| e.surface == row.surface) {
                already_covered += 1;
                continue;
            }
            if !seen.insert(row.surface.clone()) {
                continue;
            }
            // Classify directly from the dash-joined POS string to skip the
            // per-row `Vec<&str>` allocation that `classify(&[..])` requires.
            let bucket = classify_pos_string(&row.pos);
            candidates.push((
                bucket,
                Candidate {
                    reading: reading.clone(),
                    surface: row.surface,
                    cost: row.cost,
                    pos: row.pos,
                },
            ));
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

/// Mine extras candidates from a Wikipedia XML dump.
///
/// Surface-first pipeline (see `candidates::wikipedia`):
/// 1. Stream the dump, count maximal kanji runs by frequency.
/// 2. Diff against the merged build dict's surface set.
/// 3. Surfaces NOT in the build dict, with `freq >= min_freq`, are written
///    to `wikipedia.tsv` sorted by frequency descending.
///
/// Reading-assignment is intentionally skipped here. The user reviews top-N
/// surfaces and assigns readings by hand (or via a separate tool) before
/// promoting to `extras/<domain>.tsv`. This mirrors the existing
/// `mine`-then-promote-by-hand workflow.
pub fn corpus(
    dump_path: &Path,
    build_dict_path: &Path,
    out_dir: &Path,
    min_freq: u32,
) -> Result<(), CandidateError> {
    eprintln!("Scanning {} ...", dump_path.display());
    let freqs = wikipedia::extract_kanji_freqs(dump_path)?;

    let dict = TrieDictionary::open(build_dict_path).map_err(|e| {
        CandidateError::Parse(format!(
            "open build dict {}: {e}",
            build_dict_path.display()
        ))
    })?;

    // Build the build-dict surface set once. At ~1.2M entries this is ~10MB
    // of String storage; trivial vs the freq map (~few hundred MB at
    // full-corpus scale before frequency filtering).
    let mut covered: HashSet<String> = HashSet::new();
    for (_reading, entries) in dict.iter() {
        for e in entries {
            covered.insert(e.surface);
        }
    }
    eprintln!("Build dict covers {} unique surfaces.", covered.len());

    // Filter + sort: keep only surfaces NOT in build dict, with freq>=min,
    // sort by freq desc then surface asc for deterministic output.
    let mut gaps: Vec<(String, u32)> = freqs
        .into_iter()
        .filter(|(s, f)| *f >= min_freq && !covered.contains(s))
        .collect();
    gaps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    fs::create_dir_all(out_dir)?;
    let path = out_dir.join("wikipedia.tsv");
    let file = fs::File::create(&path)?;
    let mut w = BufWriter::new(file);
    writeln!(w, "# Candidate pool for the curated `extras/` layer.")?;
    writeln!(w, "# Source: Wikipedia 日本語 dump (CC-BY-SA)")?;
    writeln!(
        w,
        "# Generated by `dictool candidates corpus` — DO NOT edit manually."
    )?;
    writeln!(
        w,
        "# Surfaces NOT in the build dict, freq >= {min_freq}, sorted desc."
    )?;
    writeln!(
        w,
        "# Reading is NOT assigned — pick top-N by hand and look up readings"
    )?;
    writeln!(w, "# before promoting to extras/<domain>.tsv. Gitignored.")?;
    writeln!(w, "#")?;
    writeln!(w, "# format: surface\\tfreq")?;
    for (s, f) in &gaps {
        writeln!(w, "{s}\t{f}")?;
    }
    w.flush()?;
    eprintln!(
        "Wrote {} gap surfaces (freq >= {}) to {}",
        gaps.len(),
        min_freq,
        path.display()
    );
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
