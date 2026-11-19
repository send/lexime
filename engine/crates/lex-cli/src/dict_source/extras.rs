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
///    entries to the appropriate domain TSV here. Decide the POS by the
///    majority rule documented on [`ALLOWED_POS_TAGS`], not by carrying it
///    over, and decide the cost anew (a promoted entry's old cost is not
///    the extras value either). A proper-noun row takes the cost rule on
///    [`ALLOWED_POS_TAGS`]. A common-noun row picks a cost that is sound for
///    the word, grounded in the same reading's Mozc entries or in a
///    before/after measurement (a unit's Japanese name: the derived value
///    of "The unit-names domain").
/// 2. **Proactive mining**: run `dictool candidates mine` (see
///    `lex_cli::candidates`). It downloads SudachiDict, diffs against the
///    build dict, and writes per-bucket candidate TSVs under
///    `engine/data/extras-candidates/`. Scan those by hand and copy useful
///    rows into `extras/<domain>.tsv`. Mining output never auto-merges into
///    the build dict — promotion is always explicit. Its rows are
///    `reading\tsurface\tcost\tpos` with Sudachi's POS, which does not fit the
///    extras grammar (`reading\tsurface\tpos\tcost`, pos an id.def tag from
///    `ALLOWED_POS_TAGS`) and fails the build as is. On promotion, reorder the
///    columns, rewrite the POS to the allowed tag the majority rule picks, and
///    write the section comment (kind, representative words, counts). The
///    mining cost is on Sudachi's scale, not the extras final value; decide
///    the cost anew: a proper-noun row by the cost rule on
///    [`ALLOWED_POS_TAGS`], a common-noun row grounded in the same reading's
///    Mozc entries or in a before/after measurement.
const DOMAINS: &[(&str, &str)] = &[
    ("it.tsv", include_str!("extras/it.tsv")),
    ("food.tsv", include_str!("extras/food.tsv")),
    ("geography.tsv", include_str!("extras/geography.tsv")),
    ("history.tsv", include_str!("extras/history.tsv")),
    ("culture.tsv", include_str!("extras/culture.tsv")),
    ("units.tsv", include_str!("extras/units.tsv")),
    ("unit-names.tsv", include_str!("extras/unit-names.tsv")),
];

/// Closed set of POS tags an extras row may use: 名詞,一般, the six
/// 名詞,固有名詞 subdivisions extras has words for, and 名詞,接尾,助数詞 (the
/// `units` domain only). A tag outside the set is
/// a parse error, so a typo cannot silently match another id.def tag; a new
/// subdivision is added here in the same change as the row that needs it.
/// The numeric ids are resolved from the pinned id.def at compile time by
/// [`ExtrasSource::resolve_pos_ids`] — ids are line numbers in id.def, so a
/// hardcoded one silently degrades entries when a pin bump renumbers them
/// (#273: the old constant drifted onto 名詞,一般,あと and every entry paid a
/// +3777 BOS-connection penalty; see #279).
///
/// ## Which tag a row takes (the full rule; CLAUDE.md 設計哲学 states it short)
///
/// **One rule, for proper vs. common noun and for the subdivision alike: a
/// row takes the majority POS that Mozc (the pinned mozc-raw) gives to words
/// of the same kind. Only when Mozc has no word of that kind, or the count is
/// split, is the POS decided by the word's meaning.** The rule and the counts
/// are data in the TSVs (section comments); no code branches on a word.
///
/// - **Kind** = what the word denotes (person / organization / place /
///   country / era / work / rank / thing or concept). Kinds are not split by
///   period, region or field (Japanese and Chinese persons are one kind;
///   publishers and tea houses are one kind), except along a property that
///   the id.def subdivisions themselves distinguish. id.def's 人名 splits 姓 /
///   名 / 一般 (full name) by which part of a name the word is — not by period
///   or region — so a **full name** (姓 + 名, 曹操 = 曹 + 操) and a **title**
///   (an emperor's name such as 武帝 or 文帝: a single appellation with no
///   surname, not a 姓 + 名 pair) are different kinds. Pooling titles with
///   full names would count a kind that ignores the very property id.def
///   distinguishes.
/// - **Representative words** = words of the same kind looked up in Mozc
///   (extras' own words are not counted). Every word looked up is listed in
///   the section comment, including those Mozc does not have, so the choice
///   can be reviewed.
/// - **Counting**: for each representative word, take the set of POS of its
///   Mozc entries with that (reading, surface) and map each to a bucket. A
///   名詞,固有名詞 subdivision is one bucket as is (固有名詞,一般 / 人名,一般 /
///   人名,名 / 人名,姓 / 組織 / 地域,一般 / 地域,国 …); the five content-noun
///   classes (名詞,一般 / サ変接続 / 形容動詞語幹 / 副詞可能 / ナイ形容詞語幹)
///   are one bucket, "common noun". Each word casts one vote per bucket it
///   has (a word with several buckets votes in each, never twice in one).
///   The cheapest entry does not decide. A common noun is written 名詞,一般
///   (the finer classes are grammar — する / な — and a word that needs one
///   adds that tag here in the same change).
/// - **Entries counted as another word** (the excluded count is recorded;
///   only what the POS alone decides, on both the proper and the common
///   side): (i) a proper-noun bucket that contradicts what the kind denotes
///   — 人名 / 組織 / 地域 on an era, work, rank or thing; 人名 / 組織 on a
///   country or place; 組織 / 地域 on a person; 人名 / 地域 on an
///   organization. (ii) a grammatical class that is no name of a thing: every
///   noun subdivision other than the five common-noun classes and
///   名詞,固有名詞 (名詞,数 / 接尾 (incl. 助数詞) / 接尾可能 / 代名詞 / 非自立 /
///   特殊 / 接続詞的 / 動詞非自立的 / 引用文字列), and every non-noun.
///   **Homographs inside one bucket are not excluded** (an era's 文化 =
///   culture 名詞,サ変接続, 文明 = civilization 名詞,一般): the POS cannot
///   tell them apart, and telling them apart word by word would turn the
///   rule back into per-word judgement. They stay in the count, and the
///   section comment states whether dropping them changes the result.
/// - **Majority** (the canonical sentence; CLAUDE.md 設計哲学 states the
///   same definition):
///   「多数派 = 票を持つ代表語の数の過半の票を得て、かつ単独で最多の区分。最多が同数か、過半の票を得た区分が無ければ割れ」 —
///   with n the number of representative words that cast a vote, the bucket
///   with the most votes k, if 2k > n and no other bucket also has k. Since
///   a word may vote in several buckets, two buckets can both exceed half;
///   the strict top one wins.
/// - **Split** (a tie at the top, or no bucket over half): decide by meaning
///   — a common noun is 名詞,一般; a proper noun takes the id.def
///   subdivision whose definition fits (a full name = 人名,一般, a country =
///   地域,国, a work = 固有名詞,一般 as id.def has no subdivision for works).
/// - **Section comment** (a section = a block of rows of one kind): the kind
///   and the resulting POS, the votes (k/n and the main buckets), for a split
///   the reason the meaning gives, the number of excluded other-word
///   entries, and the representative words (including those missing from
///   Mozc). When one kind spans files, the full list sits in one file and
///   the others refer to it. Mozc upstream values are not changed.
///
/// ## The units domain (名詞,接尾,助数詞)
///
/// `extras/units.tsv` holds SI units written in Latin letters (`s`, `μs`,
/// `kB`, `Hz`, `°C`), every row 名詞,接尾,助数詞 — the POS Mozc gives unit
/// words. It is outside the majority rule above: a unit is a counter by what
/// it is, not by a vote, and the tag is added here (PR2b) because that is the
/// POS the Latin-unit rules key on (a counter × a pure-Latin surface stands
/// only after a typed digit). Its cost is not a proper-noun median nor 5000:
/// `C = c_ref + rank`, c_ref being the cost of the unit's Japanese name: in
/// Mozc, the cheapest entry of that name whose ids are both the unit's own
/// (the counter id) if there is one, else the cheapest entry; for the 10
/// readings Mozc has no name for, the name is a row of `unit-names.tsv` (next
/// section) and c_ref is its cost. Derived and stated in the TSV's header and
/// above each reading.
///
/// ## The unit-names domain (名詞,一般, derived cost)
///
/// `extras/unit-names.tsv` holds the Japanese names (ピコ秒, キロビット,
/// テラワット, ...) of the units of `units.tsv` that the pinned Mozc has no
/// entry for (10 readings, the ones `units.tsv` marks "derived"), so that a
/// unit has a name to sort behind. It is a word domain, **not** outside the
/// majority rule above: the POS is the rule's answer, 名詞,一般 (28 of the 28
/// representative words that vote are common nouns; the count and the word
/// list are the section comment in the TSV) — a counter POS here would make
/// `ここ2キロビット`-type input convert to the 助数詞 name. **The cost is
/// none of the common-noun 5000, a cost-override or a same-reading Mozc
/// entry (the ways a common-noun row's cost is otherwise decided below): it is the derived value** — the cheapest Mozc entry of the same
/// family's unit-less base plus the floored median of the (compound − base)
/// differences over the compounds Mozc has. The formula is in the header of
/// `units.tsv`; the values and their inputs are the `derived` lines of
/// `testcorpus/unit-symbol-readings.txt`, and `dictool check-units` recomputes
/// them from the dictionary, checks each name is one 名詞,一般 entry at that
/// cost and is not in Mozc's source. When the pin is bumped, derive them again
/// (that file's bump step 4).
///
/// ## The cost of a proper-noun row
///
/// The cost comes from the same sample as the POS: **cost = the median, over
/// the representative words that have a Mozc entry in the decided POS, of
/// each word's cheapest such entry's cost (per word, not per entry; the
/// fractional part of an even-n median is floored; Mozc's raw value, before
/// compile's person-name offset)**. Rows of one section (one kind) share one
/// value. Reason: POS and cost are a pair: connection costs and the cost
/// distribution of a POS go together, and the 5000 the existing common-noun
/// rows carry went with 名詞,一般, not with a proper-noun subdivision. Per
/// word, so a word with several entries in the POS (織田信長, 豊臣秀吉 in
/// 人名,一般) counts once. The raw value is stored; the person-name offset
/// (+2000) is compile's, applied to Mozc entries only (in whatever position
/// Mozc is given), so curated 人名,一般 rows are about 2000 cheaper than
/// Mozc's effective full-name value. The
/// values are counted on the mozc-raw of the pin in `engine/data/mozc-pin.txt`
/// (the sha lives only there); when the pin is bumped, derive them again
/// (that file's bump step 4). A Mozc entry's POS is read by its left id (the
/// id.def line of that id): the vote counts and the cost medians were both
/// counted by left id.
///
/// - When none of the representative words has a Mozc entry in the decided
///   POS (n = 0), the value is not given by this rule: stop and ask the user.
/// - Common-noun rows are outside the rule: an existing row keeps its
///   current value (5000, or its cost-override, or — for `unit-names.tsv` —
///   the derived value, see "The unit-names domain"), and a new row's cost is
///   decided as in "Adding entries" (grounded in the same reading's Mozc
///   entries or in a before/after measurement).
/// - A proper-noun row with a cost-override comment is outside the rule too;
///   its value is the one chosen by the measurement below, written in that
///   row's comment (the values and ranges are there, not here, so they are
///   not copied into this doc). A proper-noun row leaves the rule's value
///   only when it collides with a homophone (a cost-conflict case in the
///   accuracy corpus) and the collision was measured on the lattice. (The
///   common-noun overrides 寛保 / マシン keep their earlier records and are
///   not covered by this.)
///   - Grid: cost 0 to 8000 in steps of 100 (outside the range found by
///     bisection, every point inside it).
///   - Population of a word w: w's reading alone, the reading + each of the
///     7 particles (の が を は で と に), the 3 frames (「r のほん」「r がいった」
///     「これは r とあう」), and the Mozc readings and accuracy-corpus
///     readings that contain the reading.
///   - Judgement K: over every reading of the population, top-1 is the same
///     as before the PR. Judgement R: for w's reading alone, w is on the
///     first page (index 0 to 8).
///   - (4) = the number of candidates containing an extras word that were on
///     the first page before the PR and fall off it; (5) = the same for
///     candidates containing no extras word.
///   - Choice: within the range where K and R both hold, (4) smallest, then
///     (5) smallest, then the largest cost. An empty range, or K / R not
///     holding at every point inside it, stops the work (no choosing by eye).
///   - The full record (grid, dict sha256 per cost, choice) is frozen in
///     lexime-snapshots `unit-20261115-094814/pr1-r2` (`override/`).
/// - Section comment: the one-sentence rule, n (words, not entries), the
///   median with the min / max, whether Mozc has / votes for each word, the
///   words Mozc lacks, and that every row of the section has this value.
///   When one kind spans files, the full text sits with the representative
///   words and the others refer to it. A section that closes in one file
///   (坂, 帝号, 国) also says "Mozc's raw value (before compile's person-name
///   offset)" in its cost sentence, as the others do.
const ALLOWED_POS_TAGS: &[&str] = &[
    "名詞,一般,*,*,*,*,*",
    "名詞,固有名詞,一般,*,*,*,*",
    "名詞,固有名詞,人名,一般,*,*,*",
    "名詞,固有名詞,人名,名,*,*,*",
    "名詞,固有名詞,組織,*,*,*,*",
    "名詞,固有名詞,地域,一般,*,*,*",
    "名詞,固有名詞,地域,国,*,*,*",
    "名詞,接尾,助数詞,*,*,*,*",
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

/// The data rows of every bundled domain TSV.
#[cfg(test)]
pub(crate) fn bundled_rows() -> Vec<Vec<&'static str>> {
    DOMAINS
        .iter()
        .flat_map(|(_, content)| super::curated::test_support::data_rows(content))
        .collect()
}

/// The data rows of the bundled `units.tsv` only.
#[cfg(test)]
pub(crate) fn units_rows() -> Vec<Vec<&'static str>> {
    DOMAINS
        .iter()
        .filter(|(name, _)| *name == "units.tsv")
        .flat_map(|(_, content)| super::curated::test_support::data_rows(content))
        .collect()
}

/// The data rows of the bundled `unit-names.tsv` only.
#[cfg(test)]
pub(crate) fn unit_names_rows() -> Vec<Vec<&'static str>> {
    DOMAINS
        .iter()
        .filter(|(name, _)| *name == "unit-names.tsv")
        .flat_map(|(_, content)| super::curated::test_support::data_rows(content))
        .collect()
}

/// The data rows of every bundled domain TSV except `units.tsv` (the word
/// domains, whose tags follow the majority rule — `unit-names.tsv` is one).
#[cfg(test)]
pub(crate) fn word_rows() -> Vec<Vec<&'static str>> {
    DOMAINS
        .iter()
        .filter(|(name, _)| *name != "units.tsv")
        .flat_map(|(_, content)| super::curated::test_support::data_rows(content))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::curated::test_support::{
        self as ts, pin_id, pin_shaped_id_def, scratch, write_id_def,
    };
    use super::*;

    const IPPAN: &str = "名詞,一般,*,*,*,*,*";
    const JOSUUSHI: &str = "名詞,接尾,助数詞,*,*,*,*";

    fn parse_bundled(name: &str) -> (std::path::PathBuf, HashMap<String, Vec<DictEntry>>) {
        ts::parse_bundled(&ExtrasSource, &format!("extras_{name}"))
    }

    fn parse_err(name: &str, body: &str) -> String {
        // A registered domain name, so the file-name filter accepts it.
        ts::parse_err(&ExtrasSource, "it.tsv", &format!("extras_{name}"), body)
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

    /// T1: the bundled TSVs resolve by tag name against a pin-shaped id.def:
    /// each entry gets the pinned id of the tag its row writes (名詞,一般 =
    /// 1851, the 固有名詞 subdivisions 1920〜1929), never 1851 for a proper
    /// noun row.
    #[test]
    fn t1_bundled_resolves_by_tag_name() {
        let (dir, mut entries) = parse_bundled("t1");
        let id_def = pin_shaped_id_def(&dir);
        ExtrasSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        let rows = bundled_rows();
        for fields in &rows {
            let entry = entries[fields[0]]
                .iter()
                .find(|e| e.surface == fields[1])
                .unwrap();
            let want = pin_id(fields[2]);
            assert_eq!((entry.left_id, entry.right_id), (want, want), "{fields:?}");
        }
        let proper = rows
            .iter()
            .filter(|f| f[2] != IPPAN && f[2] != JOSUUSHI)
            .count();
        assert_eq!(proper, 25);
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
                "padded_reading",
                " あさかい\t朝会\t名詞,一般,*,*,*,*,*\t3000",
                "not hiragana",
            ),
            (
                "padded_surface",
                "あさかい\t朝会 \t名詞,一般,*,*,*,*,*\t5000",
                "leading or trailing whitespace",
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
        // 57 word rows (47 vocabulary + 10 unit names, T16n) + 75 unit rows
        // (T16 pins the unit count).
        assert_eq!(parsed, 57 + 75);
        fs::remove_dir_all(&dir).ok();
    }

    /// The rows whose POS is not 名詞,一般, by (reading, tag) — the
    /// section judgements of the TSV comments (the majority of Mozc's POS for
    /// the same kind; split kinds by meaning).
    const PROPER_NOUN_ROWS: &[(&str, &str)] = &[
        // culture: 組織
        ("しんちょうぶんこ", SOSHIKI),
        ("ちゅうこうぶんこ", SOSHIKI),
        ("ちゅうおうこうろんしんしゃ", SOSHIKI),
        ("とうきょうだいがくしゅっぱんかい", SOSHIKI),
        ("せいぶんどうしんこうしゃ", SOSHIKI),
        ("こうぶんどう", SOSHIKI),
        // culture: 書名
        ("かどかわにほんちめいだいじてん", KOYU_IPPAN),
        ("こくしだいじてん", KOYU_IPPAN),
        // culture: 全名
        ("こむろてつや", JINMEI_IPPAN),
        ("あきもとやすし", JINMEI_IPPAN),
        ("まつとうやゆみ", JINMEI_IPPAN),
        ("あむろなみえ", JINMEI_IPPAN),
        ("さわだけんじ", JINMEI_IPPAN),
        ("つつみきょうへい", JINMEI_IPPAN),
        ("まつもとたかし", JINMEI_IPPAN),
        ("みずきなな", JINMEI_IPPAN),
        ("あくゆう", JINMEI_IPPAN),
        // food: 組織
        ("いっぽどう", SOSHIKI),
        // geography: 坂
        ("きららざか", "名詞,固有名詞,地域,一般,*,*,*"),
        // history: 書名 / 称号 / 国 / 全名
        ("しょくにほんぎ", KOYU_IPPAN),
        ("かんせいちょうしゅうしょかふ", KOYU_IPPAN),
        ("せんしそうしょ", KOYU_IPPAN),
        ("ぶてい", "名詞,固有名詞,人名,名,*,*,*"),
        ("ほくぎ", "名詞,固有名詞,地域,国,*,*,*"),
        ("そうそう", JINMEI_IPPAN),
    ];
    const SOSHIKI: &str = "名詞,固有名詞,組織,*,*,*,*";
    const KOYU_IPPAN: &str = "名詞,固有名詞,一般,*,*,*,*";
    const JINMEI_IPPAN: &str = "名詞,固有名詞,人名,一般,*,*,*";

    /// T8: the tag written on each bundled word row — the 25 rows of
    /// [`PROPER_NOUN_ROWS`], 名詞,一般 on the other 32 (22 vocabulary + the
    /// 10 of `unit-names.tsv`, T8n) — and the count per tag.
    #[test]
    fn t8_bundled_tags() {
        let rows = word_rows();
        for fields in &rows {
            let want = PROPER_NOUN_ROWS
                .iter()
                .find(|(r, _)| *r == fields[0])
                .map_or(IPPAN, |&(_, tag)| tag);
            assert_eq!(fields[2], want, "{fields:?}");
        }
        let count = |tag: &str| rows.iter().filter(|f| f[2] == tag).count();
        let counts: Vec<usize> = ALLOWED_POS_TAGS.iter().map(|t| count(t)).collect();
        // ALLOWED_POS_TAGS order: 名詞,一般 / 固有名詞,一般 / 人名,一般 /
        // 人名,名 / 組織 / 地域,一般 / 地域,国.
        assert_eq!(counts, [32, 5, 10, 1, 7, 1, 1, 0]);
        assert_eq!(rows.len(), 57);
        assert!(PROPER_NOUN_ROWS
            .iter()
            .all(|(r, _)| rows.iter().filter(|f| f[0] == *r).count() == 1));
    }

    /// T8 (units): every row of `units.tsv` is 名詞,接尾,助数詞 — and no word
    /// domain uses that tag.
    #[test]
    fn t8_units_are_all_counters() {
        let units = units_rows();
        assert!(units.iter().all(|f| f[2] == JOSUUSHI), "{units:?}");
        assert!(word_rows().iter().all(|f| f[2] != JOSUUSHI));
    }

    /// T8n: every row of `unit-names.tsv` is 名詞,一般 — the majority rule's
    /// answer for the Japanese names of prefixed units (28 of the 28 voters
    /// are common nouns; the section comment holds the count) — never the
    /// 助数詞 of the units themselves.
    #[test]
    fn t8n_unit_names_are_common_nouns() {
        let names = unit_names_rows();
        assert!(names.iter().all(|f| f[2] == IPPAN), "{names:?}");
    }

    /// T16n: the number of unit-name rows — the 10 readings units.tsv marks
    /// "derived"; a silently dropped row fails here.
    #[test]
    fn t16n_unit_name_row_count() {
        assert_eq!(unit_names_rows().len(), 10);
    }

    /// T17n: a unit name is Japanese script, so it never satisfies the
    /// Latin-unit surface condition (`pure_latin`) the unit symbols in
    /// units.tsv meet (T17).
    #[test]
    fn t17n_unit_names_are_not_pure_latin() {
        for f in unit_names_rows() {
            assert!(!lex_core::converter::pure_latin(f[1]), "{f:?}");
        }
    }

    /// T15: μ is U+03BC (never U+00B5 MICRO SIGN), ° is U+00B0 (never U+00BA
    /// or U+02DA), and no surface has a full-width alphanumeric.
    #[test]
    fn t15_unit_characters() {
        let units = units_rows();
        let mut mu = 0;
        let mut degree = 0;
        for f in &units {
            let surface = f[1];
            assert!(!surface.contains('\u{00B5}'), "{f:?}");
            assert!(
                !surface.contains('\u{00BA}') && !surface.contains('\u{02DA}'),
                "{f:?}"
            );
            assert!(
                !surface
                    .chars()
                    .any(|c| ('\u{FF10}'..='\u{FF5A}').contains(&c)),
                "{f:?}"
            );
            mu += surface.matches('\u{03BC}').count();
            degree += surface.matches('\u{00B0}').count();
        }
        // Every row that has a μ or ° has the intended code point: the
        // micro readings (まいくろ…) carry μ, the temperature units °.
        let micro = units
            .iter()
            .filter(|f| f[0].starts_with("まいくろ"))
            .count();
        assert_eq!(mu, micro);
        assert_eq!(micro, 5);
        assert_eq!(degree, 4); // °C ×2 (せっし, どしー), °F ×2 (かし, どえふ)
    }

    /// T16: the number of unit rows (75 = 73 units + the 2 variant readings
    /// どしー / どえふ) — a silently dropped row fails here.
    #[test]
    fn t16_unit_row_count() {
        assert_eq!(units_rows().len(), 75);
    }

    /// T17: every unit surface meets the Latin-unit surface condition (ASCII
    /// letter, no Japanese character), by the very predicate the converter
    /// uses (`pure_latin`) — a surface outside it would not get the Latin-unit
    /// rules.
    #[test]
    fn t17_unit_surfaces_are_pure_latin() {
        for f in units_rows() {
            assert!(lex_core::converter::pure_latin(f[1]), "{f:?}");
        }
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
