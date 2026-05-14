//! Candidate mining for the curated `extras` dict layer.
//!
//! Outputs lists of `(reading, surface, ...)` tuples that exist in upstream
//! sources (e.g. SudachiDict) but NOT in our merged build dict — i.e.
//! candidates the user might want to promote into `extras/<domain>.tsv`.
//!
//! ## Why this is separate from `dict_source`
//!
//! `dict_source` modules feed entries directly into `lexime.dict` at build
//! time. We deliberately avoid that for SudachiDict (PR #156 deleted the
//! Mozc+Sudachi merge after it produced too much top-1 noise).
//!
//! `candidates` is the opposite contract: nothing here is ever merged into
//! the build dict automatically. The output sits in `engine/data/extras-candidates/`
//! (gitignored) for the user to scan and hand-promote into `extras/<domain>.tsv`.
//!
//! ## Workflow
//!
//! ```text
//! dictool candidates mine               # download + diff + classify
//! → engine/data/extras-candidates/sudachi-{common,place,other}.tsv
//!
//! grep -i "椒\|醤\|油" extras-candidates/sudachi-common.tsv | head -50
//! → review, copy promising lines to extras/food.tsv
//! → PR
//! ```
//!
//! ## Domain split
//!
//! SudachiDict's POS hierarchy doesn't carry semantic categories, so we can't
//! cleanly split into `food` / `it` / `medical` etc. The `mine` step does a
//! coarse split:
//!
//! - `sudachi-place.tsv`: `名詞,固有名詞,地名,*` — directly usable for the
//!   `geography` extras domain.
//! - `sudachi-common.tsv`: `名詞,普通名詞,一般,*` — the bulk; user grep-filters.
//! - `sudachi-other.tsv`: everything else (verbs, adjectives, particles, ...);
//!   rarely useful for extras but kept for completeness.

pub mod sudachi;
pub mod wikipedia;

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CandidateError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("parse error: {0}")]
    Parse(String),
}

/// A row produced by upstream parsing (e.g. SudachiDict). The reading is
/// kept out of this struct because it duplicates the HashMap key in
/// `parse_dir`'s output — at full-Sudachi scale that's millions of extra
/// String allocations.
#[derive(Debug, Clone)]
pub struct CandidateRow {
    pub surface: String,
    /// SudachiDict's word cost. Lower = more common in Sudachi's training data,
    /// useful as a frequency proxy when scanning the candidate list.
    pub cost: i32,
    /// Pretty-printed POS hierarchy (e.g., `名詞-普通名詞-一般`). Mostly for
    /// human review; ignored when promoting to `extras/`.
    pub pos: String,
}

/// Output-ready candidate (reading paired with a row). Constructed at the
/// promotion site in `candidates_ops::mine` from the (reading, row) pair.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub reading: String,
    pub surface: String,
    pub cost: i32,
    pub pos: String,
}

/// One of the coarse buckets the candidate file is split into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// `名詞,固有名詞,地名,*` — usable as `geography` domain seed.
    Place,
    /// `名詞,普通名詞,一般,*` — the bulk of useful extras candidates.
    Common,
    /// Anything else — verbs, adjectives, suffixes, particles, etc.
    Other,
}

impl Bucket {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Place => "sudachi-place.tsv",
            Self::Common => "sudachi-common.tsv",
            Self::Other => "sudachi-other.tsv",
        }
    }
}

/// Classify a Sudachi POS tuple into a candidate bucket.
///
/// Sudachi POS columns (CSV col 5..=10; col 4 is normalized surface, not POS):
/// `主品詞, 品詞細分類1, 品詞細分類2, 品詞細分類3, 活用型, 活用形`
pub fn classify(pos: &[&str]) -> Bucket {
    if pos.len() < 3 {
        return Bucket::Other;
    }
    match (pos[0], pos[1], pos[2]) {
        ("名詞", "固有名詞", "地名") => Bucket::Place,
        ("名詞", "普通名詞", "一般") => Bucket::Common,
        _ => Bucket::Other,
    }
}

/// Same as `classify` but takes the dash-joined POS string directly.
/// Avoids the per-row `Vec<&str>` allocation from `pos.split('-').collect()`.
pub fn classify_pos_string(pos: &str) -> Bucket {
    let mut parts = pos.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("名詞"), Some("固有名詞"), Some("地名")) => Bucket::Place,
        (Some("名詞"), Some("普通名詞"), Some("一般")) => Bucket::Common,
        _ => Bucket::Other,
    }
}

/// Write candidates to per-bucket TSV files under `dest`.
///
/// Each file gets a comment header explaining where the data came from and
/// what the columns mean, so the user can read it without context-switching
/// to docs.
pub fn write_candidates(
    dest: &Path,
    sudachi_version: &str,
    candidates: &[(Bucket, Candidate)],
) -> Result<(), CandidateError> {
    fs::create_dir_all(dest)?;

    for &bucket in &[Bucket::Place, Bucket::Common, Bucket::Other] {
        let mut sorted: Vec<&Candidate> = candidates
            .iter()
            .filter(|(b, _)| *b == bucket)
            .map(|(_, c)| c)
            .collect();
        // Lower Sudachi cost = more common = scan-priority first.
        // Compare borrowed strings — at full-Sudachi scale (~1.9M rows) the
        // sort_by_key clone variant adds millions of String allocations.
        sorted.sort_by(|a, b| {
            a.cost
                .cmp(&b.cost)
                .then_with(|| a.surface.as_str().cmp(b.surface.as_str()))
                .then_with(|| a.reading.as_str().cmp(b.reading.as_str()))
        });

        // Stream rows directly to a buffered writer instead of building a
        // single 100MB+ String first — at full-Sudachi scale that doubled
        // peak RSS (candidates Vec + giant output buffer).
        let path = dest.join(bucket.filename());
        let file = fs::File::create(&path)?;
        let mut w = BufWriter::new(file);
        // Each writeln! is its own header line — emitting the header as one
        // continued raw string would prepend leading whitespace from the
        // source-code indentation and break tooling that expects `#` at col 0.
        writeln!(w, "# Candidate pool for the curated `extras/` layer.")?;
        writeln!(w, "# Source: SudachiDict v{sudachi_version} (Apache-2.0)")?;
        writeln!(w, "# Bucket: {bucket:?}  ({} entries)", sorted.len())?;
        writeln!(
            w,
            "# Generated by `dictool candidates mine` — DO NOT edit manually."
        )?;
        writeln!(
            w,
            "# Lines NOT in the build dict; sorted by Sudachi cost ascending"
        )?;
        writeln!(
            w,
            "# (most common first). Promote useful rows to extras/<domain>.tsv"
        )?;
        writeln!(w, "# by hand. This file is gitignored.")?;
        writeln!(w, "#")?;
        writeln!(w, "# format: reading\\tsurface\\tcost\\tpos")?;
        for c in &sorted {
            writeln!(w, "{}\t{}\t{}\t{}", c.reading, c.surface, c.cost, c.pos)?;
        }
        w.flush()?;
        eprintln!("  {} ({} entries)", path.display(), sorted.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_dispatches_correctly() {
        let place = ["名詞", "固有名詞", "地名", "*", "*", "*"];
        let common = ["名詞", "普通名詞", "一般", "*", "*", "*"];
        let person = ["名詞", "固有名詞", "人名", "姓", "*", "*"];
        let verb = ["動詞", "一般", "*", "*", "五段-カ行", "終止形-一般"];

        assert_eq!(classify(&place), Bucket::Place);
        assert_eq!(classify(&common), Bucket::Common);
        assert_eq!(classify(&person), Bucket::Other);
        assert_eq!(classify(&verb), Bucket::Other);
    }

    #[test]
    fn classify_handles_short_pos() {
        assert_eq!(classify(&[]), Bucket::Other);
        assert_eq!(classify(&["名詞"]), Bucket::Other);
    }

    #[test]
    fn classify_pos_string_matches_classify() {
        assert_eq!(classify_pos_string("名詞-固有名詞-地名"), Bucket::Place);
        assert_eq!(classify_pos_string("名詞-普通名詞-一般"), Bucket::Common);
        assert_eq!(classify_pos_string("名詞-固有名詞-人名"), Bucket::Other);
        assert_eq!(classify_pos_string("動詞"), Bucket::Other);
        assert_eq!(classify_pos_string(""), Bucket::Other);
        // Trailing components (POS subtypes) are ignored — first 3 decide.
        assert_eq!(
            classify_pos_string("名詞-固有名詞-地名-一般"),
            Bucket::Place
        );
    }

    #[test]
    fn write_candidates_splits_buckets_and_sorts_by_cost() {
        let dir = std::env::temp_dir().join("lexime_test_candidates_write");
        let _ = fs::remove_dir_all(&dir);

        let cands = vec![
            (
                Bucket::Common,
                Candidate {
                    reading: "べきとう".into(),
                    surface: "冪等".into(),
                    cost: 8000,
                    pos: "名詞-普通名詞-一般".into(),
                },
            ),
            (
                Bucket::Common,
                Candidate {
                    reading: "あさかい".into(),
                    surface: "朝会".into(),
                    cost: 5000,
                    pos: "名詞-普通名詞-一般".into(),
                },
            ),
            (
                Bucket::Place,
                Candidate {
                    reading: "きららざか".into(),
                    surface: "雲母坂".into(),
                    cost: 7000,
                    pos: "名詞-固有名詞-地名".into(),
                },
            ),
        ];

        write_candidates(&dir, "20260428", &cands).unwrap();

        let common = fs::read_to_string(dir.join("sudachi-common.tsv")).unwrap();
        // Lower-cost entry comes first.
        let asakai_pos = common.find("あさかい").unwrap();
        let bekitou_pos = common.find("べきとう").unwrap();
        assert!(asakai_pos < bekitou_pos);

        let place = fs::read_to_string(dir.join("sudachi-place.tsv")).unwrap();
        assert!(place.contains("きららざか\t雲母坂"));

        let other = fs::read_to_string(dir.join("sudachi-other.tsv")).unwrap();
        assert!(other.contains("0 entries"));

        // Header lines must start with `#` at column 0 — no leading
        // whitespace from raw-string source-code indentation.
        for line in common.lines().filter(|l| !l.is_empty()) {
            if line.starts_with('#') || line.contains('\t') {
                continue;
            }
            panic!("non-header non-data line: {line:?}");
        }
        let header_count = common.lines().filter(|l| l.starts_with("# ")).count()
            + common.lines().filter(|l| l == &"#").count();
        assert!(header_count >= 8, "header lost lines: {header_count}");

        fs::remove_dir_all(&dir).ok();
    }
}
