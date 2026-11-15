use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::curated::{self, CostColumn};
use super::{parse_dict_files, DictSource, DictSourceError};
use lex_core::dict::DictEntry;

/// Curated domain-specific vocabulary not covered by Mozc UT.
///
/// Each domain TSV under `extras/<domain>.tsv` is bundled into the binary via
/// `include_str!`. `fetch` materializes them to disk so the existing `compile`
/// pipeline (which reads from a directory) can consume them without
/// special-casing — same approach as `symbols.rs`.
///
/// Domains intentionally cover broad areas (`food`, `it`, `geography`) rather
/// than narrow categories like `cooking-chinese`: a vocabulary item like
/// 一保堂(いっぽどう, 京都の日本茶ブランド) sits naturally in `food` but has
/// nowhere clean to go under a stricter taxonomy.
///
/// File format: the curated grammar ([`curated::parse_curated_line`]) with a
/// required cost column:
///   reading[TAB]surface[TAB]pos[TAB]cost
/// `pos` is a full 7-field id.def tag from [`ALLOWED_POS_TAGS`]; `cost` is
/// the entry's final cost (a non-negative i16). Neither has a default.
/// Comment lines (starting with `#`) and blank lines are skipped.
///
/// ## Adding entries
///
/// Two paths:
///
/// 1. **From friction**: when conversion fails for you, register in user_dict
///    (`dictool user-dict add`), use it for a while, then promote stable
///    entries to the appropriate domain TSV here.
/// 2. **Proactive mining**: run `dictool candidates mine` (see
///    `lex_cli::candidates`). It downloads SudachiDict, diffs against the
///    build dict, and writes per-bucket candidate TSVs under
///    `engine/data/extras-candidates/`. Scan those by hand and copy useful
///    rows into `extras/<domain>.tsv`. Mining output never auto-merges into
///    the build dict — promotion is always explicit.
const DOMAINS: &[(&str, &str)] = &[
    ("it.tsv", include_str!("extras/it.tsv")),
    ("food.tsv", include_str!("extras/food.tsv")),
    ("geography.tsv", include_str!("extras/geography.tsv")),
    ("history.tsv", include_str!("extras/history.tsv")),
    ("culture.tsv", include_str!("extras/culture.tsv")),
];

/// Closed set of POS tags an extras row may use: 名詞,一般 and the six
/// 名詞,固有名詞 subdivisions extras has words for. A tag outside the set is
/// a parse error, so a typo cannot silently match another id.def tag; a new
/// subdivision is added here in the same change as the row that needs it.
/// The numeric ids are resolved from the pinned id.def at compile time by
/// [`ExtrasSource::resolve_pos_ids`] — ids are line numbers in id.def, so a
/// hardcoded one silently degrades entries when a pin bump renumbers them
/// (#273: the old constant drifted onto 名詞,一般,あと and every entry paid a
/// +3777 BOS-connection penalty; see #279).
const ALLOWED_POS_TAGS: &[&str] = &[
    "名詞,一般,*,*,*,*,*",
    "名詞,固有名詞,一般,*,*,*,*",
    "名詞,固有名詞,人名,一般,*,*,*",
    "名詞,固有名詞,人名,名,*,*,*",
    "名詞,固有名詞,組織,*,*,*,*",
    "名詞,固有名詞,地域,一般,*,*,*",
    "名詞,固有名詞,地域,国,*,*,*",
];

pub struct ExtrasSource;

impl DictSource for ExtrasSource {
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
        parse_dict_files(
            dir,
            "extras *.tsv",
            // Only parse files that are still registered in DOMAINS. This
            // prevents stale TSVs (e.g. a renamed/removed domain whose old
            // file lingers in the gitignored extras-raw/ dir) from silently
            // leaking entries into the merged dict and making builds
            // non-deterministic. Mirrors symbols.rs's exact-name match.
            |name| DOMAINS.iter().any(|(n, _)| *n == name),
            '\t',
            |fields| {
                curated::parse_curated_line(fields, CostColumn::Required, ALLOWED_POS_TAGS)
                    .map(Some)
            },
        )
    }

    fn resolve_pos_ids(
        &self,
        entries: &mut HashMap<String, Vec<DictEntry>>,
        id_def: Option<&Path>,
    ) -> Result<(), DictSourceError> {
        curated::resolve_pos_ids(entries, id_def, ALLOWED_POS_TAGS, "extras")
    }

    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError> {
        fs::create_dir_all(dest).map_err(DictSourceError::Io)?;
        for (name, content) in DOMAINS {
            fs::write(dest.join(name), content).map_err(DictSourceError::Io)?;
            eprintln!("  {name} ({} bytes)", content.len());
        }
        fs::write(dest.join(".stamp"), "").map_err(DictSourceError::Io)?;
        eprintln!(
            "Wrote {} bundled extras TSV(s) to {}",
            DOMAINS.len(),
            dest.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::curated::test_support::{pin_shaped_id_def, scratch, write_id_def};
    use super::*;

    const IPPAN: &str = "名詞,一般,*,*,*,*,*";

    fn parse_bundled(name: &str) -> (std::path::PathBuf, HashMap<String, Vec<DictEntry>>) {
        let dir = scratch(&format!("extras_{name}"));
        ExtrasSource.fetch(&dir).unwrap();
        let entries = ExtrasSource.parse_dir(&dir).unwrap();
        (dir, entries)
    }

    fn parse_err(name: &str, body: &str) -> String {
        let dir = scratch(&format!("extras_{name}"));
        // A registered domain name, so the file-name filter accepts it.
        fs::write(dir.join("it.tsv"), body).unwrap();
        let err = ExtrasSource.parse_dir(&dir).unwrap_err().to_string();
        fs::remove_dir_all(&dir).ok();
        err
    }

    /// The data rows of every bundled domain TSV.
    fn bundled_rows() -> Vec<Vec<&'static str>> {
        DOMAINS
            .iter()
            .flat_map(|(_, content)| content.lines())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split('\t').collect())
            .collect()
    }

    #[test]
    fn bundled_tsvs_parse() {
        let (dir, entries) = parse_bundled("bundled");

        // IT domain
        let bekitou = entries.get("べきとう").expect("べきとう must map to 冪等");
        assert!(bekitou.iter().any(|e| e.surface == "冪等"));
        let asakai = entries.get("あさかい").expect("あさかい must map to 朝会");
        assert!(asakai.iter().any(|e| e.surface == "朝会"));

        // food domain
        let tanjao = entries
            .get("たんじゃお")
            .expect("たんじゃお must map to 藤椒");
        assert!(tanjao.iter().any(|e| e.surface == "藤椒"));
        let ippodo = entries
            .get("いっぽどう")
            .expect("いっぽどう must map to 一保堂");
        assert!(ippodo.iter().any(|e| e.surface == "一保堂"));

        // geography domain
        let kirarazaka = entries
            .get("きららざか")
            .expect("きららざか must map to 雲母坂");
        assert!(kirarazaka.iter().any(|e| e.surface == "雲母坂"));

        // history domain
        let eiroku = entries.get("えいろく").expect("えいろく must map to 永禄");
        assert!(eiroku.iter().any(|e| e.surface == "永禄" && e.cost == 5000));
        // Cost override on a history entry (寛保 cost=7000) must round-trip.
        let kanpou = entries.get("かんぽう").expect("かんぽう must map to 寛保");
        assert!(kanpou.iter().any(|e| e.surface == "寛保" && e.cost == 7000));

        // culture domain
        let komuro = entries
            .get("こむろてつや")
            .expect("こむろてつや must map to 小室哲哉");
        assert!(komuro.iter().any(|e| e.surface == "小室哲哉"));

        // parse_dir alone leaves the POS as a sentinel: compile always
        // follows up with resolve_pos_ids.
        for entry in entries.values().flatten() {
            assert!(!entry.symbol);
            assert!(entry.left_id > 2700 && entry.right_id > 2700, "{entry:?}");
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// T1: the bundled TSVs resolve by tag name against a pin-shaped id.def.
    /// Every row is 名詞,一般 (1851) for now.
    #[test]
    fn t1_bundled_resolves_by_tag_name() {
        let (dir, mut entries) = parse_bundled("t1");
        let id_def = pin_shaped_id_def(&dir);
        ExtrasSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        for entry in entries.values().flatten() {
            assert_eq!((entry.left_id, entry.right_id), (1851, 1851), "{entry:?}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// T2: the resolved id follows id.def, wherever the tag sits — every
    /// allowed tag resolves to its own row.
    #[test]
    fn t2_follows_renumbered_id() {
        let dir = scratch("extras_t2");
        let body: String = ALLOWED_POS_TAGS
            .iter()
            .enumerate()
            .map(|(i, tag)| format!("{}\tx{i}\t{tag}\t5000\n", "あ".repeat(i + 1)))
            .collect();
        fs::write(dir.join("it.tsv"), body).unwrap();
        let mut entries = ExtrasSource.parse_dir(&dir).unwrap();
        let overrides: Vec<(usize, &str)> = ALLOWED_POS_TAGS
            .iter()
            .enumerate()
            .map(|(i, tag)| (i + 3, *tag))
            .collect();
        let id_def = write_id_def(&dir, 12, &overrides);
        ExtrasSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        for (i, _) in ALLOWED_POS_TAGS.iter().enumerate() {
            let entry = &entries[&"あ".repeat(i + 1)][0];
            let want = (i + 3) as u16;
            assert_eq!((entry.left_id, entry.right_id), (want, want), "{entry:?}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// T3: a tag outside the allowed set is an error naming line and tag.
    #[test]
    fn t3_disallowed_tag_is_err() {
        let err = parse_err(
            "t3",
            "べきとう\t冪等\t名詞,一般,*,*,*,*,*\t5000\nほし\t★\t記号,一般,*,*,*,*,*\t5000\n",
        );
        assert!(err.contains("it.tsv:2:"), "{err}");
        assert!(err.contains("記号,一般,*,*,*,*,*"), "{err}");
        assert!(err.contains("not in the allowed set"), "{err}");
    }

    /// T4: an allowed tag missing from id.def is an error naming the tag; a
    /// vocab-specific 名詞,一般 row must not satisfy the generic tag — that
    /// near miss is exactly the #273 degradation.
    #[test]
    fn t4_tag_missing_from_id_def_is_err() {
        let (dir, mut entries) = parse_bundled("t4");
        let id_def = write_id_def(&dir, 10, &[(7, "名詞,一般,*,*,*,*,あと")]);
        let err = ExtrasSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap_err()
            .to_string();
        assert!(err.contains(IPPAN), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    /// T5: no id.def is an error, not a silent no-op.
    #[test]
    fn t5_no_id_def_is_err() {
        let err = ExtrasSource
            .resolve_pos_ids(&mut HashMap::new(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("extras requires id.def"), "{err}");
    }

    /// T6: rows that break the grammar are errors with their line number —
    /// including a missing cost (there is no default cost any more).
    #[test]
    fn t6_malformed_rows_are_err() {
        let cases = [
            (
                "no_cost",
                "あさかい\t朝会\t名詞,一般,*,*,*,*,*",
                "4 tab-separated",
            ),
            ("old_cost_only", "あさかい\t朝会\t3000", "4 tab-separated"),
            ("old_two_columns", "あさかい\t朝会", "4 tab-separated"),
            (
                "five_columns",
                "あさかい\t朝会\t名詞,一般,*,*,*,*,*\t3000\tx",
                "4 tab-separated",
            ),
            (
                "empty_cost",
                "あさかい\t朝会\t名詞,一般,*,*,*,*,*\t",
                "not an i16",
            ),
            (
                "cost_typo",
                "あさかい\t朝会\t名詞,一般,*,*,*,*,*\tabc",
                "not an i16",
            ),
            (
                "cost_over_i16",
                "あさかい\t朝会\t名詞,一般,*,*,*,*,*\t40000",
                "not an i16",
            ),
            (
                "cost_negative",
                "あさかい\t朝会\t名詞,一般,*,*,*,*,*\t-1",
                "negative",
            ),
            (
                "non_hiragana",
                "Asakai\t朝会\t名詞,一般,*,*,*,*,*\t3000",
                "not hiragana",
            ),
            (
                "empty_reading",
                "\t朝会\t名詞,一般,*,*,*,*,*\t3000",
                "not hiragana",
            ),
            (
                "empty_surface",
                "あさかい\t\t名詞,一般,*,*,*,*,*\t3000",
                "empty surface",
            ),
            (
                "padded_surface",
                " あさかい\t朝会\t名詞,一般,*,*,*,*,*\t3000",
                "not hiragana",
            ),
        ];
        for (name, row, want) in cases {
            let err = parse_err(
                &format!("t6_{name}"),
                &format!("# h\n\nべきとう\t冪等\t名詞,一般,*,*,*,*,*\t5000\n{row}\n"),
            );
            assert!(err.contains("it.tsv:4:"), "{name}: {err}");
            assert!(err.contains(want), "{name}: {err}");
        }
    }

    /// T7: every non-comment, non-blank line becomes an entry.
    #[test]
    fn t7_no_row_is_dropped() {
        let (dir, entries) = parse_bundled("t7");
        let parsed = entries.values().map(Vec::len).sum::<usize>();
        assert_eq!(parsed, bundled_rows().len());
        assert_eq!(parsed, 47);
        fs::remove_dir_all(&dir).ok();
    }

    /// T8: the tag written on each bundled row (every row 名詞,一般 for now).
    #[test]
    fn t8_bundled_tags() {
        let rows = bundled_rows();
        for fields in &rows {
            assert_eq!(fields[2], IPPAN, "{fields:?}");
        }
        assert_eq!(rows.len(), 47);
    }

    /// T10: after resolution every id is a row of id.def — no sentinel left.
    #[test]
    fn t10_resolved_ids_within_id_def() {
        let (dir, mut entries) = parse_bundled("t10");
        let id_def = pin_shaped_id_def(&dir);
        let rows = fs::read_to_string(&id_def).unwrap().lines().count();
        ExtrasSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        for entry in entries.values().flatten() {
            assert!((entry.left_id as usize) < rows && (entry.right_id as usize) < rows);
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ignores_unregistered_tsv_files() {
        // A stale TSV left over from a renamed/removed domain (or any file
        // that's not in DOMAINS) must NOT contribute entries — that would
        // make builds non-deterministic.
        let dir = scratch("extras_stale");
        fs::write(
            dir.join("cooking-chinese.tsv"),
            "ぱくちー\t香菜\t名詞,一般,*,*,*,*,*\t5000\n",
        )
        .unwrap();
        fs::write(
            dir.join("it.tsv"),
            "べきとう\t冪等\t名詞,一般,*,*,*,*,*\t5000\n",
        )
        .unwrap();

        let entries = ExtrasSource.parse_dir(&dir).unwrap();
        assert!(
            entries.contains_key("べきとう"),
            "registered file's entries must be picked up"
        );
        assert!(
            !entries.contains_key("ぱくちー"),
            "unregistered file must be ignored, not silently merged"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
