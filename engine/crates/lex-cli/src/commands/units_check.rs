//! `dictool check-units`: plan unit-symbols §7-4 T14, run on the built
//! dictionary (the one `dictool compile` writes, with the pinned id.def).
//!
//! Checks, for every PR2b entry:
//! 1. each unit (reading, surface) exists and its POS is the counter id (a
//!    duplicate merge did not swap in Mozc's POS);
//! 2. each unit's cost is `c_ref + rank`, `c_ref` being the cost of the
//!    Japanese-name entry the `[ref]` section names: among the entries of
//!    (reading, name) the cheapest one with the counter id on both sides, else
//!    the cheapest entry (aliases: the main reading's C). Derived readings
//!    (names Mozc lacks): (a) the c_ref written is the value recomputed from
//!    the dictionary (each word's cheapest entry as the input); (d) the name
//!    is in the dictionary exactly once, both ids 名詞,一般, at that cost (a
//!    row of extras' unit-names.tsv); (e) unit-names.tsv's (reading, surface)
//!    set is the `derived` lines' (reading, name) set; (g) Mozc's source has
//!    no such (reading, name) (when it does, the row goes and the line becomes
//!    a `name` line);
//! 3. each SI prefix entry has the 記号,アルファベット id;
//! 4. every entry of the symbols and extras sources has both ids below
//!    `num_ids` (the id.def row count).
//!
//! Nothing is skipped: a missing dictionary, id.def, readings file, Mozc
//! source or other source directory is a failure (plan C5). The reading-set
//! file grammar is in that file's header.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use lex_core::dict::{DictEntry, Dictionary, TrieDictionary};

use crate::dict_source::{self, pos_map, UNITS_TSV, UNIT_NAMES_TSV};

const COUNTER_TAG: &str = "名詞,接尾,助数詞,*,*,*,*";
const PREFIX_TAG: &str = "記号,アルファベット,*,*,*,*,*";
/// The POS of the Japanese names extras' unit-names.tsv adds.
const NAME_TAG: &str = "名詞,一般,*,*,*,*,*";

type Pair = (String, String);

#[derive(Debug, Default, PartialEq)]
pub struct Readings {
    /// `[S-std]` units: (reading, surface), file order.
    pub std_units: Vec<Pair>,
    /// `[S-std]` prefixes: (reading, surface).
    pub std_prefixes: Vec<Pair>,
    /// `name` lines: (reading, ref surface, unit surfaces in rank order).
    pub names: Vec<(String, String, Vec<String>)>,
    /// `alias` lines: (reading, surface, main reading).
    pub aliases: Vec<(String, String, String)>,
    /// `family` lines: id -> (base reading, compound readings).
    pub families: BTreeMap<String, (String, Vec<String>)>,
    pub derived: Vec<Derived>,
    /// The generated reading sets (`SET_SECTIONS`): section -> (reading, tag)
    /// in file order. S-snap's `file` line is kept as (path, sha256).
    pub sets: BTreeMap<String, Vec<Pair>>,
}

/// The sections of the machine-generated reading sets (plan 8-1). T14 does
/// not price them; it reads them so that a malformed set fails here, and
/// keeps them in `Readings::sets`. Every record is `r<TAB>reading<TAB>tag`;
/// S-snap's is `file<TAB>path<TAB>sha256`.
const SET_SECTIONS: &[&str] = &[
    "S-dig",
    "S-kana",
    "S-mid",
    "S-pre",
    "S-col",
    "S-dig-col",
    "S-mozc",
    "S-ni",
    "S-snap",
];

#[derive(Debug, PartialEq)]
pub struct Derived {
    pub reading: String,
    /// The Japanese name (a row of extras' unit-names.tsv).
    pub name: String,
    pub c_ref: i32,
    pub surfaces: Vec<String>,
    pub base: String,
    pub families: Vec<String>,
}

/// `pairs` as a set of borrowed (reading, surface).
fn pair_set(pairs: &[Pair]) -> HashSet<(&str, &str)> {
    pairs
        .iter()
        .map(|(r, s)| (r.as_str(), s.as_str()))
        .collect()
}

fn fields<'a>(line: &'a str, kind: &str, n: usize, lineno: usize) -> Result<Vec<&'a str>, String> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() != n || f.iter().any(|x| x.is_empty()) {
        return Err(format!(
            "line {lineno}: '{kind}' takes {n} non-empty TAB-separated fields, got {}",
            f.len()
        ));
    }
    Ok(f)
}

pub fn parse_readings(content: &str) -> Result<Readings, String> {
    let mut r = Readings::default();
    let mut section: Option<&str> = None;
    for (i, line) in content.lines().enumerate() {
        let lineno = i + 1;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(match name {
                "S-std" => "S-std",
                "ref" => "ref",
                _ => SET_SECTIONS
                    .iter()
                    .copied()
                    .find(|s| *s == name)
                    .ok_or_else(|| format!("line {lineno}: unknown section [{name}]"))?,
            });
            continue;
        }
        let kind = line.split('\t').next().unwrap_or("");
        match (section, kind) {
            (None, _) => return Err(format!("line {lineno}: record before any section")),
            (Some("S-std"), "unit") => {
                let f = fields(line, kind, 3, lineno)?;
                r.std_units.push((f[1].into(), f[2].into()));
            }
            (Some("S-std"), "prefix") => {
                let f = fields(line, kind, 3, lineno)?;
                r.std_prefixes.push((f[1].into(), f[2].into()));
            }
            (Some("ref"), "name") => {
                let f = fields(line, kind, 4, lineno)?;
                r.names.push((
                    f[1].into(),
                    f[2].into(),
                    f[3].split(',').map(String::from).collect(),
                ));
            }
            (Some("ref"), "alias") => {
                let f = fields(line, kind, 4, lineno)?;
                r.aliases.push((f[1].into(), f[2].into(), f[3].into()));
            }
            (Some("ref"), "family") => {
                let f = fields(line, kind, 4, lineno)?;
                let compounds = f[3].split(' ').map(String::from).collect();
                if r.families
                    .insert(f[1].into(), (f[2].into(), compounds))
                    .is_some()
                {
                    return Err(format!("line {lineno}: family '{}' defined twice", f[1]));
                }
            }
            (Some("ref"), "derived") => {
                let f = fields(line, kind, 7, lineno)?;
                let c_ref = f[3]
                    .parse()
                    .map_err(|_| format!("line {lineno}: c_ref '{}' is not an integer", f[3]))?;
                r.derived.push(Derived {
                    reading: f[1].into(),
                    name: f[2].into(),
                    c_ref,
                    surfaces: f[4].split(',').map(String::from).collect(),
                    base: f[5].into(),
                    families: f[6].split(',').map(String::from).collect(),
                });
            }
            (Some(s), k) if SET_SECTIONS.contains(&s) => {
                let want = if s == "S-snap" { "file" } else { "r" };
                if k != want {
                    return Err(format!("line {lineno}: unknown record '{k}' in [{s}]"));
                }
                let f = fields(line, k, 3, lineno)?;
                r.sets
                    .entry(s.into())
                    .or_default()
                    .push((f[1].into(), f[2].into()));
            }
            (Some(s), k) => {
                return Err(format!("line {lineno}: unknown record '{k}' in [{s}]"));
            }
        }
    }
    if r.std_units.is_empty() || r.std_prefixes.is_empty() || r.names.is_empty() {
        return Err("a section is empty or missing ([S-std] units/prefixes, [ref] names)".into());
    }
    Ok(r)
}

/// Median of `v` with an even count floored (toward -inf), as the cost rule
/// in units.tsv says.
pub fn floor_median(v: &[i32]) -> i32 {
    let mut s = v.to_vec();
    s.sort_unstable();
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]).div_euclid(2)
    }
}

/// The POS ids the check needs, from the pinned id.def.
pub struct Ids {
    pub counter: u16,
    pub prefix: u16,
    /// 名詞,一般 (the POS of the unit-names.tsv rows).
    pub name: u16,
    pub num_ids: u32,
}

pub fn read_ids(id_def: &Path) -> Result<Ids, String> {
    let def = pos_map::parse_id_def(id_def).map_err(|e| format!("id.def: {e}"))?;
    let tag = |t: &str| {
        def.tags
            .get(t)
            .copied()
            .ok_or_else(|| format!("'{t}' not in {}", id_def.display()))
    };
    let max = def.max_id.ok_or("id.def has no rows")?;
    if usize::from(max) + 1 != def.rows {
        return Err(format!(
            "id.def ids are not 0..rows (max id {max}, {} rows)",
            def.rows
        ));
    }
    Ok(Ids {
        counter: tag(COUNTER_TAG)?,
        prefix: tag(PREFIX_TAG)?,
        name: tag(NAME_TAG)?,
        num_ids: def.rows as u32,
    })
}

/// Entries of `reading` with `surface`.
fn with_surface(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    reading: &str,
    surface: &str,
) -> Vec<DictEntry> {
    lookup(reading)
        .into_iter()
        .filter(|e| e.surface == surface)
        .collect()
}

/// The unit entry (counter POS on both sides) of (reading, surface).
fn unit_entry(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    ids: &Ids,
    reading: &str,
    surface: &str,
) -> Result<DictEntry, String> {
    let found = with_surface(lookup, reading, surface);
    if found.is_empty() {
        return Err(format!("({reading}, {surface}) is not in the dictionary"));
    }
    found
        .iter()
        .find(|e| e.left_id == ids.counter && e.right_id == ids.counter)
        .cloned()
        .ok_or_else(|| {
            let got: Vec<String> = found
                .iter()
                .map(|e| format!("L={} R={}", e.left_id, e.right_id))
                .collect();
            format!(
                "({reading}, {surface}) has no 助数詞 entry (want id {}, got {})",
                ids.counter,
                got.join(" / ")
            )
        })
}

/// The lowest cost among `entries`.
fn min_cost<'a>(entries: impl Iterator<Item = &'a DictEntry>) -> Option<i32> {
    entries.map(|e| i32::from(e.cost)).min()
}

/// The cheapest entry of (reading, surface), POS not considered — the input
/// of a derived reading's recomputation (each word's cheapest entry).
fn cheapest_cost(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    reading: &str,
    surface: &str,
) -> Result<i32, String> {
    min_cost(with_surface(lookup, reading, surface).iter())
        .ok_or_else(|| format!("ref ({reading}, {surface}) is not in the dictionary"))
}

/// c_ref: the cheapest entry of (reading, ref surface) with the counter id on
/// both sides (the unit's own id) if there is one, else the cheapest entry
/// with ids not considered. The input of `rank_check`.
fn ref_cost(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    ids: &Ids,
    reading: &str,
    surface: &str,
) -> Result<i32, String> {
    let found = with_surface(lookup, reading, surface);
    min_cost(
        found
            .iter()
            .filter(|e| e.left_id == ids.counter && e.right_id == ids.counter),
    )
    .or_else(|| min_cost(found.iter()))
    .ok_or_else(|| format!("ref ({reading}, {surface}) is not in the dictionary"))
}

/// Run T14 (1)-(4). `units_tsv` / `unit_names` are the (reading, surface)
/// pairs of the extras `units.tsv` / `unit-names.tsv` that went into the
/// compile; `curated` is every (reading, surface) of the symbols and extras
/// sources; `mozc` is the (reading, surface) pairs of the pinned Mozc source
/// (at least those of the `derived` readings). Returns every failure (empty =
/// pass).
pub fn check(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    ids: &Ids,
    readings: &Readings,
    units_tsv: &[Pair],
    unit_names: &[Pair],
    curated: &[Pair],
    mozc: &[Pair],
) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();

    // The three places that list the units must agree: S-std, the units.tsv
    // that was compiled, and the ref section (each pair priced exactly once,
    // surfaces in the same order).
    let tsv = pair_set(units_tsv);
    let std_set = pair_set(&readings.std_units);
    if std_set.len() != readings.std_units.len() {
        bad.push("[S-std] lists a unit pair twice".into());
    }
    for (r, s) in std_set.difference(&tsv) {
        bad.push(format!("S-std unit ({r}, {s}) is not in units.tsv"));
    }
    for (r, s) in tsv.difference(&std_set) {
        bad.push(format!("units.tsv ({r}, {s}) is not in [S-std]"));
    }
    let mut std_by_reading: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (r, s) in &readings.std_units {
        std_by_reading.entry(r).or_default().push(s);
    }
    let mut priced: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (r, _, ss) in &readings.names {
        priced
            .entry(r)
            .or_default()
            .extend(ss.iter().map(String::as_str));
    }
    for (r, s, _) in &readings.aliases {
        priced.entry(r).or_default().push(s);
    }
    for d in &readings.derived {
        priced
            .entry(&d.reading)
            .or_default()
            .extend(d.surfaces.iter().map(String::as_str));
    }
    if priced != std_by_reading {
        bad.push(
            "the [ref] section's readings/surfaces (name + alias + derived) differ from [S-std]'s units"
                .into(),
        );
    }

    // (1) existence and POS of every unit; the unit's C by (reading, surface).
    let mut c_of: HashMap<(&str, &str), i32> = HashMap::new();
    for (r, s) in &readings.std_units {
        match unit_entry(lookup, ids, r, s) {
            Ok(e) => {
                c_of.insert((r, s), i32::from(e.cost));
            }
            Err(e) => bad.push(format!("(1) {e}")),
        }
    }

    // (2) C - c_ref == rank, per kind.
    let name_of: HashMap<&str, &str> = readings
        .names
        .iter()
        .map(|(r, n, _)| (r.as_str(), n.as_str()))
        .collect();
    // The recomputation's input: each word's cheapest entry.
    let cheapest_of = |reading: &str| -> Result<i32, String> {
        let n = name_of
            .get(reading)
            .ok_or_else(|| format!("no `name` line for family member '{reading}'"))?;
        cheapest_cost(lookup, reading, n)
    };
    let rank_check = |reading: &str, surfaces: &[String], c_ref: i32| {
        let mut out = Vec::new();
        for (i, s) in surfaces.iter().enumerate() {
            let rank = i as i32 + 1;
            if let Some(c) = c_of.get(&(reading, s.as_str())) {
                if c - c_ref != rank {
                    out.push(format!(
                        "(2) ({reading}, {s}): C {c} - c_ref {c_ref} = {}, rank {rank}",
                        c - c_ref
                    ));
                }
            }
        }
        out
    };
    for (r, n, ss) in &readings.names {
        match ref_cost(lookup, ids, r, n) {
            Ok(c_ref) => bad.extend(rank_check(r, ss, c_ref)),
            Err(e) => bad.push(format!("(2) {e}")),
        }
    }
    for (r, s, main) in &readings.aliases {
        match (
            c_of.get(&(r.as_str(), s.as_str())),
            c_of.get(&(main.as_str(), s.as_str())),
        ) {
            (Some(a), Some(b)) if a != b => bad.push(format!(
                "(2) alias ({r}, {s}) C {a} != main ({main}, {s}) C {b}"
            )),
            (Some(_), Some(_)) => {}
            _ => bad.push(format!(
                "(2) alias ({r}, {s}) or its main ({main}) has no unit entry"
            )),
        }
    }
    for d in &readings.derived {
        // (2d) the Japanese name is in the dictionary exactly once, with
        // 名詞,一般 on both sides, at the c_ref written here.
        let named = with_surface(lookup, &d.reading, &d.name);
        match named.as_slice() {
            [] => bad.push(format!(
                "(2d) derived {}: name ({}, {}) is not in the dictionary",
                d.reading, d.reading, d.name
            )),
            [one] => {
                if one.left_id != ids.name || one.right_id != ids.name {
                    bad.push(format!(
                        "(2d) derived {}: name {} has ids L={} R={}, want 名詞,一般 (id {})",
                        d.reading, d.name, one.left_id, one.right_id, ids.name
                    ));
                }
                if i32::from(one.cost) != d.c_ref {
                    bad.push(format!(
                        "(2d) derived {}: name {} costs {} but c_ref is {}",
                        d.reading, d.name, one.cost, d.c_ref
                    ));
                }
            }
            many => bad.push(format!(
                "(2d) derived {}: name ({}, {}) has {} entries, want exactly 1",
                d.reading,
                d.reading,
                d.name,
                many.len()
            )),
        }
        // The unit's rank is priced from the c_ref the dictionary gives (the
        // same route as a `name` line). No entry at all was reported above.
        if let Ok(c_ref) = ref_cost(lookup, ids, &d.reading, &d.name) {
            bad.extend(rank_check(&d.reading, &d.surfaces, c_ref));
        }
        let before = bad.len();
        let fail = |bad: &mut Vec<String>, e: String| {
            bad.push(format!("(2) derived {}: {e}", d.reading));
        };
        let base = match cheapest_of(&d.base) {
            Ok(b) => b,
            Err(e) => {
                fail(&mut bad, e);
                continue;
            }
        };
        let mut diffs: Vec<i32> = Vec::new();
        for fid in &d.families {
            let Some((fbase, members)) = readings.families.get(fid) else {
                fail(&mut bad, format!("unknown family '{fid}'"));
                continue;
            };
            let fb = match cheapest_of(fbase) {
                Ok(c) => c,
                Err(e) => {
                    fail(&mut bad, e);
                    continue;
                }
            };
            for m in members {
                match cheapest_of(m) {
                    Ok(c) => diffs.push(c - fb),
                    Err(e) => fail(&mut bad, e),
                }
            }
        }
        if bad.len() != before || diffs.is_empty() {
            continue;
        }
        let want = base + floor_median(&diffs);
        if want != d.c_ref {
            bad.push(format!(
                "(2) derived {}: c_ref written {} but the dictionary gives {} (base {} + median over {} differences)",
                d.reading,
                d.c_ref,
                want,
                base,
                diffs.len()
            ));
        }
    }

    // (2e) unit-names.tsv and the `derived` lines list the same
    // (reading, name) pairs.
    let names_tsv = pair_set(unit_names);
    let names_derived: HashSet<(&str, &str)> = readings
        .derived
        .iter()
        .map(|d| (d.reading.as_str(), d.name.as_str()))
        .collect();
    for (r, s) in names_tsv.difference(&names_derived) {
        bad.push(format!(
            "(2e) unit-names.tsv ({r}, {s}) has no `derived` line"
        ));
    }
    for (r, s) in names_derived.difference(&names_tsv) {
        bad.push(format!(
            "(2e) `derived` ({r}, {s}) is not in unit-names.tsv"
        ));
    }

    // (2g) Mozc has none of the derived names (whatever its cost or ids).
    let mozc_set = pair_set(mozc);
    for (r, s) in names_derived.intersection(&mozc_set) {
        bad.push(format!(
            "(2g) Mozc has ({r}, {s}): delete its unit-names.tsv row, turn the `derived` line into a `name` line and count again"
        ));
    }

    // (3) prefixes.
    for (r, s) in &readings.std_prefixes {
        let found = with_surface(lookup, r, s);
        if !found
            .iter()
            .any(|e| e.left_id == ids.prefix && e.right_id == ids.prefix)
        {
            bad.push(format!(
                "(3) prefix ({r}, {s}) has no 記号,アルファベット entry (id {}; found {} entries with the surface)",
                ids.prefix,
                found.len()
            ));
        }
    }

    // (4) every symbols/extras entry exists with both ids below num_ids.
    // `curated::resolve_pos_ids` guarantees this by construction (it replaces
    // every sentinel with an id.def row id); this check confirms that
    // invariant end to end, through the merge and the serialization into the
    // built dictionary.
    for (r, s) in curated {
        let found = with_surface(lookup, r, s);
        if found.is_empty() {
            bad.push(format!("(4) curated ({r}, {s}) is not in the dictionary"));
        }
        for e in found {
            if u32::from(e.left_id) >= ids.num_ids || u32::from(e.right_id) >= ids.num_ids {
                bad.push(format!(
                    "(4) ({r}, {s}) ids L={} R={} not below num_ids {}",
                    e.left_id, e.right_id, ids.num_ids
                ));
            }
        }
    }
    bad
}

/// The raw parse of the source `name` in `dir`.
fn parse_source(name: &str, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, String> {
    let src = dict_source::from_name(name).ok_or_else(|| format!("no source '{name}'"))?;
    src.parse_dir(dir)
        .map_err(|e| format!("parsing {name} in {}: {e}", dir.display()))
}

/// Every (reading, surface) of a parsed source.
fn pairs_of(parsed: impl IntoIterator<Item = (String, Vec<DictEntry>)>) -> Vec<Pair> {
    parsed
        .into_iter()
        .flat_map(|(r, list)| list.into_iter().map(move |e| (r.clone(), e.surface)))
        .collect()
}

fn curated_pairs(name: &str, dir: &Path) -> Result<Vec<Pair>, String> {
    Ok(pairs_of(parse_source(name, dir)?))
}

/// The (reading, surface) pairs of the pinned Mozc source in `dir`, only for
/// `readings` (the whole source is ~700k entries and only the derived names
/// are asked about). A directory with no `dictionary*.txt` is an error.
fn mozc_pairs(dir: &Path, readings: &HashSet<&str>) -> Result<Vec<Pair>, String> {
    Ok(pairs_of(
        parse_source("mozc", dir)?
            .into_iter()
            .filter(|(r, _)| readings.contains(r.as_str())),
    ))
}

/// CLI entry. Prints each failure and returns `Err` (exit 1) on any.
pub fn run(
    dict_file: &str,
    id_def: &str,
    readings_file: &str,
    symbols_dir: &str,
    extras_dir: &str,
    mozc_dir: &str,
) -> Result<(), String> {
    let dict = TrieDictionary::open(Path::new(dict_file))
        .map_err(|e| format!("opening dictionary {dict_file}: {e}"))?;
    let ids = read_ids(Path::new(id_def))?;
    let content = fs::read_to_string(readings_file)
        .map_err(|e| format!("reading set {readings_file}: {e}"))?;
    let readings = parse_readings(&content).map_err(|e| format!("{readings_file}: {e}"))?;
    let units_tsv = dict_source::parse_extras_domain(Path::new(extras_dir), UNITS_TSV)?;
    let unit_names = dict_source::parse_extras_domain(Path::new(extras_dir), UNIT_NAMES_TSV)?;
    let mut curated = curated_pairs("symbols", Path::new(symbols_dir))?;
    curated.extend(curated_pairs("extras", Path::new(extras_dir))?);

    let derived_readings: HashSet<&str> = readings
        .derived
        .iter()
        .map(|d| d.reading.as_str())
        .collect();
    let mozc = mozc_pairs(Path::new(mozc_dir), &derived_readings)?;

    let bad = check(
        &|r| dict.lookup(r),
        &ids,
        &readings,
        &units_tsv,
        &unit_names,
        &curated,
        &mozc,
    );
    if bad.is_empty() {
        eprintln!(
            "check-units: ok ({} units, {} prefixes, {} curated entries, num_ids {})",
            readings.std_units.len(),
            readings.std_prefixes.len(),
            curated.len(),
            ids.num_ids
        );
        Ok(())
    } else {
        for b in &bad {
            eprintln!("check-units: FAIL {b}");
        }
        Err(format!("{} check-units failure(s)", bad.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict_source::test_support::scratch;

    const COUNTER: u16 = 2011;
    const ALPHA: u16 = 2643;
    const IPPAN: u16 = 1851;

    fn e(surface: &str, cost: i16, id: u16) -> DictEntry {
        DictEntry {
            surface: surface.into(),
            cost,
            left_id: id,
            right_id: id,
            symbol: false,
        }
    }

    const READINGS: &str = "\
[S-std]
unit\tびょう\ts
unit\tなのびょう\tns
unit\tびっと\tbit
unit\tびっと\tb
unit\tどしー\t°C
unit\tせっし\t°C
unit\tぴこびょう\tps
prefix\tなの\tn
[ref]
name\tびょう\t秒\ts
name\tなのびょう\tナノ秒\tns
name\tびっと\tビット\tbit,b
name\tせっし\t摂氏\t°C
alias\tどしー\t°C\tせっし
family\tびょう\tびょう\tなのびょう
derived\tぴこびょう\tピコ秒\t7000\tps\tびょう\tびょう
";

    fn world() -> HashMap<String, Vec<DictEntry>> {
        let mut m: HashMap<String, Vec<DictEntry>> = HashMap::new();
        let mut put = |r: &str, v: Vec<DictEntry>| {
            m.insert(r.into(), v);
        };
        put(
            "びょう",
            vec![e("秒", 2500, COUNTER), e("s", 2501, COUNTER)],
        );
        put(
            "なのびょう",
            vec![e("ナノ秒", 4500, IPPAN), e("ns", 4501, COUNTER)],
        );
        put(
            "びっと",
            vec![
                e("ビット", 3000, IPPAN),
                e("bit", 3001, COUNTER),
                e("b", 3002, COUNTER),
            ],
        );
        put(
            "せっし",
            vec![e("摂氏", 6000, IPPAN), e("°C", 6001, COUNTER)],
        );
        put("どしー", vec![e("°C", 6001, COUNTER)]);
        // derived: 2500 + median(4500 - 2500) = 4500; its name is a
        // 名詞,一般 row (unit-names.tsv) and the unit stands at 4501.
        put(
            "ぴこびょう",
            vec![e("ピコ秒", 4500, IPPAN), e("ps", 4501, COUNTER)],
        );
        put("なの", vec![e("n", 0, ALPHA)]);
        m
    }

    fn ids() -> Ids {
        Ids {
            counter: COUNTER,
            prefix: ALPHA,
            name: IPPAN,
            num_ids: 2672,
        }
    }

    fn run_check(
        w: &HashMap<String, Vec<DictEntry>>,
        readings: &str,
        tsv: &[Pair],
        curated: &[Pair],
    ) -> Vec<String> {
        run_check_with(w, readings, tsv, &unit_names(), curated, &[])
    }

    fn check_world(w: &HashMap<String, Vec<DictEntry>>) -> Vec<String> {
        run_check(w, &readings_ok(), &tsv(), &curated())
    }

    fn run_check_with(
        w: &HashMap<String, Vec<DictEntry>>,
        readings: &str,
        tsv: &[Pair],
        names: &[Pair],
        curated: &[Pair],
        mozc: &[Pair],
    ) -> Vec<String> {
        let rd = parse_readings(readings).unwrap();
        check(
            &|r| w.get(r).cloned().unwrap_or_default(),
            &ids(),
            &rd,
            tsv,
            names,
            curated,
            mozc,
        )
    }

    fn unit_names() -> Vec<Pair> {
        vec![("ぴこびょう".into(), "ピコ秒".into())]
    }

    fn tsv() -> Vec<Pair> {
        [
            ("びょう", "s"),
            ("なのびょう", "ns"),
            ("びっと", "bit"),
            ("びっと", "b"),
            ("どしー", "°C"),
            ("せっし", "°C"),
            ("ぴこびょう", "ps"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
    }

    fn readings_ok() -> String {
        // derived c_ref = 2500 + median(4500 - 2500) = 4500; ps C = 4501.
        READINGS.replace("7000", "4500")
    }

    fn curated() -> Vec<Pair> {
        vec![("なの".into(), "n".into()), ("びょう".into(), "s".into())]
    }

    #[test]
    fn passes_on_a_consistent_world() {
        let bad = check_world(&world());
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn cost_off_by_one_fails_2() {
        let mut w = world();
        w.get_mut("びょう").unwrap()[1].cost += 1;
        let bad = check_world(&w);
        assert!(
            bad.iter().any(|b| b.starts_with("(2) (びょう, s)")),
            "{bad:?}"
        );
    }

    #[test]
    fn ref_cost_change_fails_2() {
        let mut w = world();
        w.get_mut("びょう").unwrap()[0].cost += 1;
        assert!(!check_world(&w).is_empty());
    }

    #[test]
    fn swapped_pos_fails_1() {
        let mut w = world();
        w.get_mut("びょう").unwrap()[1] = e("s", 2501, IPPAN);
        let bad = check_world(&w);
        assert!(bad.iter().any(|b| b.starts_with("(1)")), "{bad:?}");
    }

    #[test]
    fn alias_drift_fails_2() {
        let mut w = world();
        w.get_mut("どしー").unwrap()[0].cost += 1;
        let bad = check_world(&w);
        assert!(bad.iter().any(|b| b.contains("alias")), "{bad:?}");
    }

    #[test]
    fn derived_value_is_recomputed() {
        let bad = run_check(&world(), READINGS, &tsv(), &curated());
        assert!(
            bad.iter().any(|b| b.contains("derived ぴこびょう")),
            "{bad:?}"
        );
    }

    #[test]
    fn bit_before_b_rank() {
        let mut w = world();
        w.get_mut("びっと").unwrap()[2].cost = 3001; // b at rank 1's price
        let bad = check_world(&w);
        assert!(
            bad.iter().any(|b| b.starts_with("(2) (びっと, b)")),
            "{bad:?}"
        );
    }

    #[test]
    fn prefix_pos_fails_3() {
        let mut w = world();
        w.get_mut("なの").unwrap()[0] = e("n", 0, COUNTER);
        let bad = check_world(&w);
        assert!(bad.iter().any(|b| b.starts_with("(3)")), "{bad:?}");
    }

    #[test]
    fn id_out_of_range_fails_4() {
        let mut w = world();
        w.get_mut("なの").unwrap()[0] = e("n", 0, 2672);
        let bad = check_world(&w);
        assert!(bad.iter().any(|b| b.starts_with("(4)")), "{bad:?}");
    }

    #[test]
    fn missing_curated_entry_fails_4() {
        let mut c = curated();
        c.push(("ない".into(), "x".into()));
        let bad = run_check(&world(), &readings_ok(), &tsv(), &c);
        assert!(bad.iter().any(|b| b.starts_with("(4)")), "{bad:?}");
    }

    #[test]
    fn unit_lists_must_agree() {
        let mut t = tsv();
        t.pop();
        let bad = run_check(&world(), &readings_ok(), &t, &curated());
        assert!(
            bad.iter().any(|b| b.contains("not in units.tsv")),
            "{bad:?}"
        );
    }

    // c_ref = the same-id (counter) entry when there is one, even if a
    // cheaper entry of another id exists (nm / V in the real data).
    #[test]
    fn c_ref_prefers_the_same_id_entry() {
        let mut w = world();
        w.insert(
            "せっし".into(),
            vec![
                e("摂氏", 5900, IPPAN),
                e("摂氏", 6000, COUNTER),
                e("°C", 6001, COUNTER),
            ],
        );
        w.insert("どしー".into(), vec![e("°C", 6001, COUNTER)]);
        let bad = check_world(&w);
        assert!(bad.is_empty(), "{bad:?}");
        // The cheapest (5900) would price the unit at 5901: that now fails.
        w.get_mut("せっし").unwrap()[2].cost = 5901;
        w.insert("どしー".into(), vec![e("°C", 5901, COUNTER)]);
        let bad = check_world(&w);
        assert!(
            bad.iter().any(|b| b.starts_with("(2) (せっし, °C)")),
            "{bad:?}"
        );
    }

    // No same-id entry: the cheapest entry, ids not considered.
    #[test]
    fn c_ref_falls_back_to_the_cheapest() {
        let mut w = world();
        w.insert(
            "せっし".into(),
            vec![
                e("摂氏", 6100, IPPAN),
                e("摂氏", 6000, 1920),
                e("°C", 6001, COUNTER),
            ],
        );
        let bad = check_world(&w);
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn derived_unit_is_priced_from_the_dictionary_name() {
        let mut w = world();
        w.get_mut("ぴこびょう").unwrap()[1].cost = 4502;
        let bad = check_world(&w);
        assert!(
            bad.iter().any(|b| b.starts_with("(2) (ぴこびょう, ps)")),
            "{bad:?}"
        );
    }

    #[test]
    fn derived_name_missing_fails_2d() {
        let mut w = world();
        w.get_mut("ぴこびょう").unwrap().remove(0);
        let bad = check_world(&w);
        assert!(bad.iter().any(|b| b.starts_with("(2d)")), "{bad:?}");
    }

    #[test]
    fn derived_name_as_counter_fails_2d() {
        // rev 9.10's shape (助数詞 on both sides) is no longer the one.
        let mut w = world();
        w.get_mut("ぴこびょう").unwrap()[0] = e("ピコ秒", 4500, COUNTER);
        let bad = check_world(&w);
        assert!(
            bad.iter()
                .any(|b| b.starts_with("(2d)") && b.contains("ids")),
            "{bad:?}"
        );
    }

    #[test]
    fn derived_name_with_one_side_ippan_fails_2d() {
        let mut w = world();
        for (left, right) in [(IPPAN, COUNTER), (COUNTER, IPPAN)] {
            let mut half = e("ピコ秒", 4500, IPPAN);
            (half.left_id, half.right_id) = (left, right);
            w.get_mut("ぴこびょう").unwrap()[0] = half;
            let bad = check_world(&w);
            assert!(
                bad.iter()
                    .any(|b| b.starts_with("(2d)") && b.contains("ids")),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn derived_name_twice_fails_2d() {
        let mut w = world();
        w.get_mut("ぴこびょう")
            .unwrap()
            .push(e("ピコ秒", 4500, 1920));
        let bad = check_world(&w);
        assert!(
            bad.iter()
                .any(|b| b.starts_with("(2d)") && b.contains("exactly 1")),
            "{bad:?}"
        );
    }

    #[test]
    fn derived_name_cost_off_fails_2d() {
        let mut w = world();
        w.get_mut("ぴこびょう").unwrap()[0].cost = 4400;
        let bad = check_world(&w);
        assert!(
            bad.iter()
                .any(|b| b.starts_with("(2d)") && b.contains("costs")),
            "{bad:?}"
        );
    }

    // The recomputation takes each word's cheapest entry: a dearer entry of
    // the unit's own id in a family member does not move the derived value.
    #[test]
    fn derivation_input_is_the_cheapest_entry() {
        let mut w = world();
        w.get_mut("なのびょう")
            .unwrap()
            .insert(1, e("ナノ秒", 6000, COUNTER));
        // ns now stands behind the same-id name (6000).
        w.get_mut("なのびょう").unwrap()[2].cost = 6001;
        let bad = check_world(&w);
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn unit_names_and_derived_must_agree_2e() {
        let w = world();
        let extra = vec![
            ("ぴこびょう".into(), "ピコ秒".into()),
            ("てらびょう".into(), "テラ秒".into()),
        ];
        let bad = run_check_with(&w, &readings_ok(), &tsv(), &extra, &curated(), &[]);
        assert!(
            bad.iter()
                .any(|b| b.starts_with("(2e)") && b.contains("no `derived` line")),
            "{bad:?}"
        );
        let bad = run_check_with(&w, &readings_ok(), &tsv(), &[], &curated(), &[]);
        assert!(
            bad.iter()
                .any(|b| b.starts_with("(2e)") && b.contains("not in unit-names.tsv")),
            "{bad:?}"
        );
    }

    #[test]
    fn derived_name_in_mozc_fails_2g() {
        let pair = vec![("ぴこびょう".to_string(), "ピコ秒".to_string())];
        let bad = run_check_with(
            &world(),
            &readings_ok(),
            &tsv(),
            &unit_names(),
            &curated(),
            &pair,
        );
        assert!(bad.iter().any(|b| b.starts_with("(2g)")), "{bad:?}");
        // Another surface of the same reading is not the name.
        let other = vec![("ぴこびょう".to_string(), "ピコビョウ".to_string())];
        let bad = run_check_with(
            &world(),
            &readings_ok(),
            &tsv(),
            &unit_names(),
            &curated(),
            &other,
        );
        assert!(bad.is_empty(), "{bad:?}");
    }

    // Mozc's cost for the name does not matter (cheap or dear), only that the
    // (reading, surface) is in the source; a missing / empty source fails.
    #[test]
    fn mozc_source_is_read_whatever_the_cost() {
        let want: HashSet<&str> = ["ぴこびょう"].into_iter().collect();
        for cost in [100, 30000] {
            let dir = scratch(&format!("units_check_cost{cost}"));
            fs::write(
                dir.join("dictionary00.txt"),
                format!("ぴこびょう\t{IPPAN}\t{IPPAN}\t{cost}\tピコ秒\n"),
            )
            .unwrap();
            let pairs = mozc_pairs(&dir, &want).unwrap();
            assert_eq!(
                pairs,
                vec![("ぴこびょう".to_string(), "ピコ秒".to_string())]
            );
            let bad = run_check_with(
                &world(),
                &readings_ok(),
                &tsv(),
                &unit_names(),
                &curated(),
                &pairs,
            );
            assert!(bad.iter().any(|b| b.starts_with("(2g)")), "{bad:?}");
            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn missing_or_empty_mozc_dir_fails() {
        let want: HashSet<&str> = ["ぴこびょう"].into_iter().collect();
        let missing = scratch("units_check_no_such_dir");
        fs::remove_dir_all(&missing).unwrap();
        assert!(mozc_pairs(&missing, &want).is_err());
        let empty = scratch("units_check_empty");
        fs::write(empty.join("unrelated.txt"), "x").unwrap();
        assert!(mozc_pairs(&empty, &want).is_err());
        fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn median_is_floored() {
        assert_eq!(floor_median(&[1]), 1);
        assert_eq!(floor_median(&[3, 1, 2]), 2);
        assert_eq!(floor_median(&[2423, 2926, 3967, 4614]), 3446);
        assert_eq!(floor_median(&[-3, -2]), -3);
    }

    #[test]
    fn parser_rejects_malformed_files() {
        assert!(parse_readings("unit\tあ\tb\n").is_err());
        assert!(parse_readings("[nope]\n").is_err());
        assert!(parse_readings("[S-std]\nunit\tあ\n").is_err());
        assert!(parse_readings("[S-std]\nbogus\tあ\tb\n").is_err());
        assert!(parse_readings("[S-std]\n").is_err());
        // A generated set takes only its own record kind, 3 fields.
        let head = "[S-std]\nunit\tあ\tb\nprefix\tあ\tb\n[ref]\nname\tあ\tb\tb\n";
        assert!(parse_readings(&format!("{head}[S-dig]\nr\t3あ\tu\n")).is_ok());
        assert!(parse_readings(&format!("{head}[S-dig]\nr\t3あ\n")).is_err());
        assert!(parse_readings(&format!("{head}[S-dig]\nunit\t3あ\tu\n")).is_err());
        assert!(parse_readings(&format!("{head}[S-snap]\nr\ta\tb\n")).is_err());
        assert!(parse_readings(&format!("{head}[S-snap]\nfile\ta\tb\n")).is_ok());
    }

    /// The committed reading set parses and has the plan's counts (69
    /// readings / 75 unit pairs, 10 prefixes, 10 derived, 2 aliases).
    #[test]
    fn committed_reading_set_shape() {
        let content = include_str!("../../../../testcorpus/unit-symbol-readings.txt");
        let r = parse_readings(content).unwrap();
        assert_eq!(r.std_units.len(), 75);
        assert_eq!(r.std_prefixes.len(), 10);
        assert_eq!(r.derived.len(), 10);
        // Each derived reading names its Japanese name, and those are the
        // rows of the bundled unit-names.tsv.
        let tsv_pairs: HashSet<Pair> = dict_source::bundled_unit_names_rows()
            .iter()
            .map(|f| (f[0].to_string(), f[1].to_string()))
            .collect();
        let derived_pairs: HashSet<Pair> = r
            .derived
            .iter()
            .map(|d| (d.reading.clone(), d.name.clone()))
            .collect();
        assert_eq!(derived_pairs, tsv_pairs);
        assert_eq!(r.aliases.len(), 2);
        assert_eq!(r.names.len(), 57);
        // The generated sets (plan 8-1; S-ctx is not made, see the PR notes).
        for sec in SET_SECTIONS {
            assert!(r.sets.get(*sec).is_some_and(|v| !v.is_empty()), "{sec}");
        }
        assert_eq!(r.sets["S-mid"].len(), 20);
        assert_eq!(r.sets["S-snap"].len(), 1);
    }
}
