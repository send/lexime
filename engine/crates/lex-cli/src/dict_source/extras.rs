use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{is_hiragana, parse_dict_files, pos_map, DictSource, DictSourceError, ParsedLine};
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
/// File format:
///   reading[TAB]surface[TAB]cost(optional, default 5000)
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

/// Default cost for entries that don't specify one. Mid-range so curated
/// entries are plausible candidates without dominating Mozc's better-tuned
/// homophones unless we explicitly want them to.
const DEFAULT_COST: i16 = 5000;

/// POS tag assigned to every extras entry: 名詞,一般 works for content nouns
/// and brand-name proper nouns alike. Per-domain POS tuning is deferred —
/// cost adjustment is sufficient for the MVP.
///
/// The numeric id for this tag is resolved from the pinned id.def at compile
/// time by [`ExtrasSource::resolve_pos_ids`] — ids are line numbers in
/// id.def, so hardcoding one silently degrades every extras entry when a pin
/// bump renumbers them (#273: the old constant drifted onto 名詞,一般,あと
/// and every entry paid a +3777 BOS-connection penalty; see #279).
const DEFAULT_POS_TAG: &str = "名詞,一般,*,*,*,*,*";

/// Placeholder POS id emitted by `parse_dir`, replaced by `resolve_pos_ids`.
/// Deliberately out of id.def's range (~2700 ids) so an unresolved entry
/// that somehow reaches a compiled dict fails loudly in conversion instead
/// of blending in as a plausible POS.
const POS_UNRESOLVED: u16 = u16::MAX;

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
                if fields.len() < 2 {
                    return None;
                }
                let reading = fields[0].trim();
                let surface = fields[1].trim();
                if reading.is_empty() || surface.is_empty() {
                    return None;
                }
                if !is_hiragana(reading) {
                    return None;
                }
                // Cost is optional — empty / missing falls back to DEFAULT_COST,
                // but a present-but-malformed value (typo, out-of-i16-range)
                // skips the line so authoring mistakes don't silently demote
                // the entry to default ranking. Mirrors parse_id_cost's
                // strictness for the other sources.
                let cost = match fields.get(2).map(|s| s.trim()) {
                    None | Some("") => DEFAULT_COST,
                    Some(s) => s.parse::<i16>().ok()?,
                };
                Some(ParsedLine {
                    reading: reading.to_string(),
                    surface: surface.to_string(),
                    left_id: POS_UNRESOLVED,
                    right_id: POS_UNRESOLVED,
                    cost,
                })
            },
        )
    }

    fn resolve_pos_ids(
        &self,
        entries: &mut HashMap<String, Vec<DictEntry>>,
        id_def: Option<&Path>,
    ) -> Result<(), DictSourceError> {
        let id_def = id_def.ok_or_else(|| {
            DictSourceError::Parse(
                "extras requires id.def to resolve its POS id — pass --id-def or place id.def in the primary input dir".to_string(),
            )
        })?;
        let pos = resolve_default_pos(id_def)?;
        eprintln!("  extras POS id: {pos} ({DEFAULT_POS_TAG})");
        for entry_list in entries.values_mut() {
            for entry in entry_list.iter_mut() {
                entry.left_id = pos;
                entry.right_id = pos;
            }
        }
        Ok(())
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

/// Look up [`DEFAULT_POS_TAG`] in `id.def`. Fails when the tag is absent —
/// a silent fallback here is exactly the drift #279 exists to prevent.
fn resolve_default_pos(id_def_path: &Path) -> Result<u16, DictSourceError> {
    let map = pos_map::parse_mozc_id_def(id_def_path)?;
    map.get(DEFAULT_POS_TAG).copied().ok_or_else(|| {
        DictSourceError::Parse(format!(
            "'{DEFAULT_POS_TAG}' not found in {} — cannot assign the extras POS id",
            id_def_path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tsvs_parse() {
        let dir = std::env::temp_dir().join("lexime_test_extras");
        let _ = fs::remove_dir_all(&dir);

        let source = ExtrasSource;
        source.fetch(&dir).unwrap();
        let entries = source.parse_dir(&dir).unwrap();

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
        assert!(eiroku.iter().any(|e| e.surface == "永禄"));
        // Cost override on a history entry (寛保 cost=7000) must round-trip.
        let kanpou = entries.get("かんぽう").expect("かんぽう must map to 寛保");
        assert!(kanpou.iter().any(|e| e.surface == "寛保" && e.cost == 7000));

        // culture domain
        let komuro = entries
            .get("こむろてつや")
            .expect("こむろてつや must map to 小室哲哉");
        assert!(komuro.iter().any(|e| e.surface == "小室哲哉"));

        // parse_dir alone leaves the POS unresolved; the compile pipeline
        // always follows up with resolve_pos_ids.
        for entry_list in entries.values() {
            for entry in entry_list {
                assert_eq!(entry.left_id, POS_UNRESOLVED);
                assert_eq!(entry.right_id, POS_UNRESOLVED);
            }
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// Write a minimal id.def with the given lines and return its path.
    fn write_id_def(dir_name: &str, lines: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.def");
        fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn resolve_pos_ids_assigns_id_from_id_def() {
        let id_def = write_id_def(
            "lexime_test_extras_pos_ok",
            &[
                "0 BOS/EOS,*,*,*,*,*,*",
                "1850 名詞,一般,*,*,*,*,あと",
                "1851 名詞,一般,*,*,*,*,*",
                "1852 名詞,サ変接続,*,*,*,*,*",
            ],
        );

        let mut entries = HashMap::from([(
            "べきとう".to_string(),
            vec![DictEntry {
                surface: "冪等".to_string(),
                cost: 5000,
                left_id: POS_UNRESOLVED,
                right_id: POS_UNRESOLVED,
            }],
        )]);

        ExtrasSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        let entry = &entries["べきとう"][0];
        assert_eq!(entry.left_id, 1851);
        assert_eq!(entry.right_id, 1851);

        fs::remove_dir_all(id_def.parent().unwrap()).ok();
    }

    #[test]
    fn resolve_pos_ids_fails_without_id_def() {
        let err = ExtrasSource
            .resolve_pos_ids(&mut HashMap::new(), None)
            .unwrap_err();
        assert!(
            err.to_string().contains("id.def"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_pos_ids_fails_when_tag_missing() {
        // A vocab-specific 名詞,一般 row must NOT satisfy the generic tag —
        // that near-miss is exactly the #273 degradation.
        let id_def = write_id_def(
            "lexime_test_extras_pos_missing",
            &[
                "0 BOS/EOS,*,*,*,*,*,*",
                "1850 名詞,一般,*,*,*,*,あと",
                "1852 名詞,サ変接続,*,*,*,*,*",
            ],
        );

        let err = ExtrasSource
            .resolve_pos_ids(&mut HashMap::new(), Some(&id_def))
            .unwrap_err();
        assert!(
            err.to_string().contains(DEFAULT_POS_TAG),
            "unexpected error: {err}"
        );

        fs::remove_dir_all(id_def.parent().unwrap()).ok();
    }

    #[test]
    fn skips_non_hiragana_reading() {
        let dir = std::env::temp_dir().join("lexime_test_extras_invalid");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Use a real registered domain name so the strict file-name filter
        // accepts it; the test still validates per-line filtering.
        fs::write(
            dir.join("food.tsv"),
            "Tanjao\t藤椒\n\
             たんじゃお\t藤椒\n",
        )
        .unwrap();

        let entries = ExtrasSource.parse_dir(&dir).unwrap();
        assert_eq!(entries.len(), 1, "non-hiragana reading must be skipped");
        assert!(entries.contains_key("たんじゃお"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explicit_cost_overrides_default() {
        let dir = std::env::temp_dir().join("lexime_test_extras_cost");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("it.tsv"), "あさかい\t朝会\t3000\n").unwrap();

        let entries = ExtrasSource.parse_dir(&dir).unwrap();
        let asakai = entries.get("あさかい").unwrap();
        assert_eq!(asakai[0].cost, 3000);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ignores_unregistered_tsv_files() {
        // A stale TSV left over from a renamed/removed domain (or any file
        // that's not in DOMAINS) must NOT contribute entries — that would
        // make builds non-deterministic.
        let dir = std::env::temp_dir().join("lexime_test_extras_stale");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Pretend a 'cooking-chinese.tsv' (no longer registered) lingers
        // alongside a registered domain file.
        fs::write(dir.join("cooking-chinese.tsv"), "ぱくちー\t香菜\n").unwrap();
        fs::write(dir.join("it.tsv"), "べきとう\t冪等\n").unwrap();

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

    #[test]
    fn skips_line_on_unparsable_cost() {
        // Out-of-i16-range value (40000 > i16::MAX = 32767) and a non-numeric
        // typo must skip the line, not fall back to default. Empty / missing
        // cost stays as default-cost behavior (covered by bundled_tsvs_parse).
        let dir = std::env::temp_dir().join("lexime_test_extras_bad_cost");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("it.tsv"),
            "あさかい\t朝会\t40000\n\
             べきとう\t冪等\tabc\n\
             しかかり\t仕掛\n",
        )
        .unwrap();

        let entries = ExtrasSource.parse_dir(&dir).unwrap();
        // Out-of-range and non-numeric lines are dropped.
        assert!(!entries.contains_key("あさかい"));
        assert!(!entries.contains_key("べきとう"));
        // Missing-cost line still produces an entry with default cost.
        let shikakari = entries
            .get("しかかり")
            .expect("missing-cost line keeps entry");
        assert_eq!(shikakari[0].cost, DEFAULT_COST);

        fs::remove_dir_all(&dir).ok();
    }
}
