//! `dictool check-units`: plan unit-symbols §7-4 T14, run on the built
//! dictionary (the one `dictool compile` writes, with the pinned id.def).
//!
//! Checks, for every PR2b entry:
//! 1. each unit (reading, surface) exists and its POS is the counter id (a
//!    duplicate merge did not swap in Mozc's POS);
//! 2. each unit's cost is `c_ref + rank`, `c_ref` being the cost of the
//!    Japanese-name entry the `[ref]` section names (derived readings: the
//!    value recomputed from the dictionary; aliases: the main reading's C);
//! 3. each SI prefix entry has the 記号,アルファベット id;
//! 4. every entry of the symbols and extras sources has both ids below
//!    `num_ids` (the id.def row count).
//!
//! Nothing is skipped: a missing dictionary, id.def, readings file or source
//! directory is a failure (plan C5). The reading-set file grammar is in that
//! file's header.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use lex_core::dict::{DictEntry, Dictionary, TrieDictionary};

use crate::dict_source::{self, pos_map};

const COUNTER_TAG: &str = "名詞,接尾,助数詞,*,*,*,*";
const PREFIX_TAG: &str = "記号,アルファベット,*,*,*,*,*";

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
}

#[derive(Debug, PartialEq)]
pub struct Derived {
    pub reading: String,
    pub c_ref: i32,
    pub surfaces: Vec<String>,
    pub base: String,
    pub families: Vec<String>,
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
            match name {
                "S-std" => section = Some("S-std"),
                "ref" => section = Some("ref"),
                _ => return Err(format!("line {lineno}: unknown section [{name}]")),
            }
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
                let f = fields(line, kind, 6, lineno)?;
                let c_ref = f[2]
                    .parse()
                    .map_err(|_| format!("line {lineno}: c_ref '{}' is not an integer", f[2]))?;
                r.derived.push(Derived {
                    reading: f[1].into(),
                    c_ref,
                    surfaces: f[3].split(',').map(String::from).collect(),
                    base: f[4].into(),
                    families: f[5].split(',').map(String::from).collect(),
                });
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
    pub num_ids: u32,
}

pub fn read_ids(id_def: &Path) -> Result<Ids, String> {
    let map = pos_map::parse_mozc_id_def(id_def).map_err(|e| format!("id.def: {e}"))?;
    let tag = |t: &str| {
        map.get(t)
            .copied()
            .ok_or_else(|| format!("'{t}' not in {}", id_def.display()))
    };
    let content = fs::read_to_string(id_def).map_err(|e| format!("{}: {e}", id_def.display()))?;
    let rows = content.lines().filter(|l| !l.trim().is_empty()).count();
    let max = content
        .lines()
        .filter_map(|l| l.split_once(' ')?.0.parse::<u32>().ok())
        .max()
        .ok_or("id.def has no rows")?;
    if max as usize + 1 != rows {
        return Err(format!(
            "id.def ids are not 0..rows (max id {max}, {rows} rows)"
        ));
    }
    Ok(Ids {
        counter: tag(COUNTER_TAG)?,
        prefix: tag(PREFIX_TAG)?,
        num_ids: rows as u32,
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

/// c_ref: the cheapest entry of (reading, ref surface), POS not considered.
fn ref_cost(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    reading: &str,
    surface: &str,
) -> Result<i32, String> {
    with_surface(lookup, reading, surface)
        .iter()
        .map(|e| i32::from(e.cost))
        .min()
        .ok_or_else(|| format!("ref ({reading}, {surface}) is not in the dictionary"))
}

/// Run T14 (1)-(4). `units_tsv` is the (reading, surface) pairs of the
/// extras `units.tsv` that went into the compile; `curated` is every
/// (reading, surface) of the symbols and extras sources. Returns every
/// failure (empty = pass).
pub fn check(
    lookup: &dyn Fn(&str) -> Vec<DictEntry>,
    ids: &Ids,
    readings: &Readings,
    units_tsv: &[Pair],
    curated: &[Pair],
) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();

    // The three places that list the units must agree: S-std, the units.tsv
    // that was compiled, and the ref section (each pair priced exactly once,
    // surfaces in the same order).
    let std: Vec<&Pair> = readings.std_units.iter().collect();
    let tsv: HashSet<&Pair> = units_tsv.iter().collect();
    let std_set: HashSet<&Pair> = std.iter().copied().collect();
    if std_set.len() != std.len() {
        bad.push("[S-std] lists a unit pair twice".into());
    }
    for p in &std_set {
        if !tsv.contains(*p) {
            bad.push(format!("S-std unit ({}, {}) is not in units.tsv", p.0, p.1));
        }
    }
    for p in &tsv {
        if !std_set.contains(*p) {
            bad.push(format!("units.tsv ({}, {}) is not in [S-std]", p.0, p.1));
        }
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
    let c_ref_of = |reading: &str| -> Result<i32, String> {
        let n = name_of
            .get(reading)
            .ok_or_else(|| format!("no `name` line for family member '{reading}'"))?;
        ref_cost(lookup, reading, n)
    };
    let rank_check =
        |reading: &str, surfaces: &[String], c_ref: i32, c_of: &HashMap<(&str, &str), i32>| {
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
        match ref_cost(lookup, r, n) {
            Ok(c_ref) => bad.extend(rank_check(r, ss, c_ref, &c_of)),
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
        let mut diffs: Vec<i32> = Vec::new();
        let mut ok = true;
        let base = match c_ref_of(&d.base) {
            Ok(b) => b,
            Err(e) => {
                bad.push(format!("(2) derived {}: {e}", d.reading));
                continue;
            }
        };
        for fid in &d.families {
            let Some((fbase, members)) = readings.families.get(fid) else {
                bad.push(format!("(2) derived {}: unknown family '{fid}'", d.reading));
                ok = false;
                continue;
            };
            let fb = match c_ref_of(fbase) {
                Ok(c) => c,
                Err(e) => {
                    bad.push(format!("(2) derived {}: {e}", d.reading));
                    ok = false;
                    continue;
                }
            };
            for m in members {
                match c_ref_of(m) {
                    Ok(c) => diffs.push(c - fb),
                    Err(e) => {
                        bad.push(format!("(2) derived {}: {e}", d.reading));
                        ok = false;
                    }
                }
            }
        }
        if !ok || diffs.is_empty() {
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
        bad.extend(rank_check(&d.reading, &d.surfaces, d.c_ref, &c_of));
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

fn units_tsv_pairs(extras_dir: &Path) -> Result<Vec<Pair>, String> {
    let path = extras_dir.join("units.tsv");
    let content = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect())
}

fn curated_pairs(name: &str, dir: &Path) -> Result<Vec<Pair>, String> {
    let src = dict_source::from_name(name).ok_or_else(|| format!("no source '{name}'"))?;
    let parsed = src
        .parse_dir(dir)
        .map_err(|e| format!("parsing {name} in {}: {e}", dir.display()))?;
    Ok(parsed
        .into_iter()
        .flat_map(|(r, list)| list.into_iter().map(move |e| (r.clone(), e.surface)))
        .collect())
}

/// CLI entry. Prints each failure and returns `Err` (exit 1) on any.
pub fn run(
    dict_file: &str,
    id_def: &str,
    readings_file: &str,
    symbols_dir: &str,
    extras_dir: &str,
) -> Result<(), String> {
    let dict = TrieDictionary::open(Path::new(dict_file))
        .map_err(|e| format!("opening dictionary {dict_file}: {e}"))?;
    let ids = read_ids(Path::new(id_def))?;
    let content = fs::read_to_string(readings_file)
        .map_err(|e| format!("reading set {readings_file}: {e}"))?;
    let readings = parse_readings(&content).map_err(|e| format!("{readings_file}: {e}"))?;
    let units_tsv = units_tsv_pairs(Path::new(extras_dir))?;
    let mut curated = curated_pairs("symbols", Path::new(symbols_dir))?;
    curated.extend(curated_pairs("extras", Path::new(extras_dir))?);

    let bad = check(&|r| dict.lookup(r), &ids, &readings, &units_tsv, &curated);
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

    const COUNTER: u16 = 2011;
    const ALPHA: u16 = 2643;

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
derived\tぴこびょう\t7000\tps\tびょう\tびょう
";

    fn world() -> HashMap<String, Vec<DictEntry>> {
        let mut m: HashMap<String, Vec<DictEntry>> = HashMap::new();
        let mut put = |r: &str, v: Vec<DictEntry>| {
            m.insert(r.into(), v);
        };
        put("びょう", vec![e("秒", 2500, 2011), e("s", 2501, COUNTER)]);
        put(
            "なのびょう",
            vec![e("ナノ秒", 4500, 1851), e("ns", 4501, COUNTER)],
        );
        put(
            "びっと",
            vec![
                e("ビット", 3000, 1851),
                e("bit", 3001, COUNTER),
                e("b", 3002, COUNTER),
            ],
        );
        put(
            "せっし",
            vec![e("摂氏", 6000, 1851), e("°C", 6001, COUNTER)],
        );
        put("どしー", vec![e("°C", 6001, COUNTER)]);
        // derived: 2500 + (4500 - 2500) = 4500 -> wait, median of one diff.
        put("ぴこびょう", vec![e("ps", 4501, COUNTER)]);
        put("なの", vec![e("n", 0, ALPHA)]);
        m
    }

    fn ids() -> Ids {
        Ids {
            counter: COUNTER,
            prefix: ALPHA,
            num_ids: 2672,
        }
    }

    fn run_check(
        w: &HashMap<String, Vec<DictEntry>>,
        readings: &str,
        tsv: &[Pair],
        curated: &[Pair],
    ) -> Vec<String> {
        let rd = parse_readings(readings).unwrap();
        check(
            &|r| w.get(r).cloned().unwrap_or_default(),
            &ids(),
            &rd,
            tsv,
            curated,
        )
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
        let bad = run_check(&world(), &readings_ok(), &tsv(), &curated());
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn cost_off_by_one_fails_2() {
        let mut w = world();
        w.get_mut("びょう").unwrap()[1].cost += 1;
        let bad = run_check(&w, &readings_ok(), &tsv(), &curated());
        assert!(
            bad.iter().any(|b| b.starts_with("(2) (びょう, s)")),
            "{bad:?}"
        );
    }

    #[test]
    fn ref_cost_change_fails_2() {
        let mut w = world();
        w.get_mut("びょう").unwrap()[0].cost += 1;
        assert!(!run_check(&w, &readings_ok(), &tsv(), &curated()).is_empty());
    }

    #[test]
    fn swapped_pos_fails_1() {
        let mut w = world();
        w.get_mut("びょう").unwrap()[1] = e("s", 2501, 1851);
        let bad = run_check(&w, &readings_ok(), &tsv(), &curated());
        assert!(bad.iter().any(|b| b.starts_with("(1)")), "{bad:?}");
    }

    #[test]
    fn alias_drift_fails_2() {
        let mut w = world();
        w.get_mut("どしー").unwrap()[0].cost += 1;
        let bad = run_check(&w, &readings_ok(), &tsv(), &curated());
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
        let bad = run_check(&w, &readings_ok(), &tsv(), &curated());
        assert!(
            bad.iter().any(|b| b.starts_with("(2) (びっと, b)")),
            "{bad:?}"
        );
    }

    #[test]
    fn prefix_pos_fails_3() {
        let mut w = world();
        w.get_mut("なの").unwrap()[0] = e("n", 0, 2011);
        let bad = run_check(&w, &readings_ok(), &tsv(), &curated());
        assert!(bad.iter().any(|b| b.starts_with("(3)")), "{bad:?}");
    }

    #[test]
    fn id_out_of_range_fails_4() {
        let mut w = world();
        w.get_mut("なの").unwrap()[0] = e("n", 0, 2672);
        let bad = run_check(&w, &readings_ok(), &tsv(), &curated());
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
        assert_eq!(r.aliases.len(), 2);
        assert_eq!(r.names.len(), 57);
    }
}
