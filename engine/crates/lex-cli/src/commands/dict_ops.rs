use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::dict_source::{self, pos_map};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::{DictEntry, Dictionary, TrieDictionary};

macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

/// Fetch raw dictionary files. With `sha`, the snapshot is pinned to that
/// exact upstream commit (`fetch_pinned` — only the mozc source supports
/// this); without it, the source's default resolution applies (mozc: latest
/// upstream master commit, used for explicit pin bumps).
pub fn fetch(source_name: &str, output_dir: &str, sha: Option<&str>) {
    let output_dir = Path::new(output_dir);
    let dict_source = dict_source::from_name(source_name).unwrap_or_else(|| {
        eprintln!(
            "Error: unknown source '{source_name}' (available: {})",
            dict_source::available_sources()
        );
        process::exit(1);
    });
    let result = match sha {
        Some(sha) => dict_source.fetch_pinned(output_dir, sha),
        None => dict_source.fetch(output_dir),
    };
    die!(result, "Error fetching dictionary: {}");
}

/// Cost offsets applied at dictionary compile time to eliminate reranker heuristics.
const PERSON_NAME_COST_OFFSET: i16 = 2000;
const PRONOUN_COST_OFFSET: i16 = -3500;
const NON_INDEPENDENT_KANJI_COST_OFFSET: i16 = 1500;

pub fn compile(
    source_name: &str,
    input_dir: &str,
    output_file: &str,
    id_def: Option<&str>,
    extra_sources: &[(String, String)],
) {
    let entries = die!(
        build_entries(source_name, input_dir, id_def, extra_sources),
        "Error: {}"
    );

    let reading_count = entries.len();
    let entry_count: usize = entries.values().map(|v| v.len()).sum();

    eprintln!("Building trie from {reading_count} readings ({entry_count} entries)...");

    let dict = TrieDictionary::from_entries(entries);
    die!(
        dict.save(Path::new(output_file)),
        "Error writing dictionary: {}"
    );

    let file_size = fs::metadata(output_file).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "Wrote {output_file} ({:.1} MB)",
        file_size as f64 / 1_048_576.0
    );
}

type EntryMap = HashMap<String, Vec<DictEntry>>;

/// Parse every source and assemble the entries `compile` writes.
///
/// Lattice sources (the primary source, then each extra source's
/// non-symbol entries) merge with the cheapest-wins rule. Symbol entries
/// (`DictEntry::symbol`, set by the symbols source) never go through that
/// rule: they are added afterwards, and a symbol whose (reading, surface)
/// is also a lattice-source entry is an error — so symbols versus lattice
/// words do not depend on the order of `--extra-source`. Among the symbols
/// of one reading, row order is arrival order (source order, then file order).
pub(crate) fn build_entries(
    source_name: &str,
    input_dir: &str,
    id_def: Option<&str>,
    extra_sources: &[(String, String)],
) -> Result<EntryMap, String> {
    let dict_source = dict_source::from_name(source_name).ok_or_else(|| {
        format!(
            "unknown source '{source_name}' (available: {})",
            dict_source::available_sources()
        )
    })?;

    let input_path = Path::new(input_dir);
    if !input_path.is_dir() {
        return Err(format!("{input_dir} is not a directory"));
    }

    // id.def drives both morpheme-role cost offsets and tag-based POS
    // resolution (symbols, extras). Auto-detect in input_dir if --id-def is not
    // specified.
    let id_def_path = id_def.map(PathBuf::from).or_else(|| {
        let auto = input_path.join("id.def");
        if auto.is_file() {
            eprintln!("Auto-detected id.def at {}", auto.display());
            Some(auto)
        } else {
            None
        }
    });

    eprintln!("Source: {source_name}");
    let mut entries = dict_source
        .parse_dir(input_path)
        .map_err(|e| format!("parsing dictionary: {e}"))?;
    dict_source
        .resolve_pos_ids(&mut entries, id_def_path.as_deref())
        .map_err(|e| format!("resolving POS ids: {e}"))?;
    let mut symbols = take_symbols(&mut entries);

    // Parse extra sources up front, but defer merging until after the
    // role-based cost offsets below. The curated sources (symbols, extras)
    // carry final costs; the offsets (person name / pronoun /
    // non-independent kanji) apply to the primary source's entries
    // (`--source`, mozc in every mise task) and never to the extra sources:
    // extra sources (symbols, extras) are merged after the offsets, so their
    // costs are final — including an extras row whose POS is a person name.
    let mut extras: Vec<(String, EntryMap)> = Vec::new();
    for (extra_name, extra_dir) in extra_sources {
        let extra_src = dict_source::from_name(extra_name).ok_or_else(|| {
            format!(
                "unknown extra source '{extra_name}' (available: {})",
                dict_source::available_sources()
            )
        })?;
        let extra_path = Path::new(extra_dir);
        if !extra_path.is_dir() {
            return Err(format!("extra input {extra_dir} is not a directory"));
        }
        eprintln!("Extra source: {extra_name}");
        let mut parsed = extra_src
            .parse_dir(extra_path)
            .map_err(|e| format!("parsing extra dictionary: {e}"))?;
        extra_src
            .resolve_pos_ids(&mut parsed, id_def_path.as_deref())
            .map_err(|e| format!("resolving POS ids for '{extra_name}': {e}"))?;
        for (reading, list) in take_symbols(&mut parsed) {
            symbols.entry(reading).or_default().extend(list);
        }
        extras.push((extra_name.clone(), parsed));
    }

    // Apply compile-time cost offsets based on morpheme roles.
    if let Some(id_def_path) = &id_def_path {
        let roles = pos_map::morpheme_roles(id_def_path)
            .map_err(|e| format!("loading morpheme roles: {e}"))?;
        let mut adjusted = 0usize;
        for entry_list in entries.values_mut() {
            for entry in entry_list.iter_mut() {
                let id = entry.left_id as usize;
                if id >= roles.len() {
                    eprintln!(
                        "Warning: left_id {} out of roles table range ({}), skipping entry '{}'",
                        id,
                        roles.len(),
                        entry.surface
                    );
                    continue;
                }
                let role = roles[id];
                let offset = match role {
                    pos_map::ROLE_PERSON_NAME => PERSON_NAME_COST_OFFSET,
                    pos_map::ROLE_PRONOUN => PRONOUN_COST_OFFSET,
                    pos_map::ROLE_NON_INDEPENDENT
                        if entry.surface.chars().any(lex_core::unicode::is_kanji) =>
                    {
                        NON_INDEPENDENT_KANJI_COST_OFFSET
                    }
                    _ => continue,
                };
                entry.cost = entry.cost.saturating_add(offset);
                adjusted += 1;
            }
        }
        eprintln!("Adjusted {adjusted} entries (person_name: +{PERSON_NAME_COST_OFFSET}, pronoun: {PRONOUN_COST_OFFSET}, non_independent_kanji: +{NON_INDEPENDENT_KANJI_COST_OFFSET})");
    }

    for (extra_name, extra_entries) in extras {
        let mut added_readings = 0usize;
        let mut added_entries = 0usize;
        let mut replaced_entries = 0usize;
        for (reading, list) in extra_entries {
            let slot = entries.entry(reading).or_default();
            if slot.is_empty() {
                added_readings += 1;
            }
            for entry in list {
                if let Some(existing) = slot.iter_mut().find(|e| e.surface == entry.surface) {
                    if entry.cost < existing.cost {
                        *existing = entry;
                        replaced_entries += 1;
                    }
                } else {
                    slot.push(entry);
                    added_entries += 1;
                }
            }
        }
        eprintln!(
            "Merged '{extra_name}': +{added_readings} readings, +{added_entries} entries, {replaced_entries} replaced"
        );
    }

    let added = add_symbols(&mut entries, symbols)?;
    eprintln!("Added {added} symbol entries (outside the lattice)");

    Ok(entries)
}

/// Move the symbol entries out of `entries` (dropping readings left empty).
fn take_symbols(entries: &mut EntryMap) -> EntryMap {
    let mut symbols = EntryMap::new();
    entries.retain(|reading, list| {
        if !list.iter().any(|e| e.symbol) {
            return !list.is_empty();
        }
        let (syms, words): (Vec<_>, Vec<_>) = list.drain(..).partition(|e| e.symbol);
        symbols.insert(reading.clone(), syms);
        *list = words;
        !list.is_empty()
    });
    symbols
}

/// Add symbol entries to the merged lattice-source entries. A symbol whose
/// (reading, surface) is already present — in a lattice source or earlier in
/// the symbols — is an error naming every such pair: a symbol never
/// overrides or shadows a lattice word, and nothing is dropped silently.
fn add_symbols(entries: &mut EntryMap, symbols: EntryMap) -> Result<usize, String> {
    let mut duplicates: Vec<(String, String)> = Vec::new();
    let mut added = 0usize;
    for (reading, list) in symbols {
        let slot = entries.entry(reading.clone()).or_default();
        // The row-order cost: arrival order among the reading's symbols, so
        // symbols from several sources (compile) or from both dictionaries
        // (merge) number 0..n in the order they arrive.
        let mut row = slot.iter().filter(|e| e.symbol).count();
        for mut entry in list {
            if slot.iter().any(|e| e.surface == entry.surface) {
                duplicates.push((reading.clone(), entry.surface));
            } else {
                entry.cost = i16::try_from(row)
                    .map_err(|_| format!("too many symbols for reading {reading:?}"))?;
                row += 1;
                slot.push(entry);
                added += 1;
            }
        }
    }
    if duplicates.is_empty() {
        return Ok(added);
    }
    duplicates.sort();
    let pairs: Vec<String> = duplicates
        .iter()
        .map(|(r, s)| format!("({r}, {s})"))
        .collect();
    Err(format!(
        "symbol (reading, surface) already in a lattice source or repeated in symbols — remove the symbols row: {}",
        pairs.join(", ")
    ))
}

pub fn compile_conn(input_txt: &str, output_file: &str, id_def: Option<&str>) {
    let text = die!(
        fs::read_to_string(input_txt),
        "Error reading {input_txt}: {}"
    );

    let (fw_min, fw_max, roles) = if let Some(id_def_path) = id_def {
        let (min, max) = die!(
            pos_map::function_word_id_range(Path::new(id_def_path)),
            "Error extracting function-word range: {}"
        );
        eprintln!("Function-word ID range: {min}..={max}");
        let roles = die!(
            pos_map::morpheme_roles(Path::new(id_def_path)),
            "Error extracting morpheme roles: {}"
        );
        let suffix_count = roles.iter().filter(|&&r| r == 2).count();
        let prefix_count = roles.iter().filter(|&&r| r == 3).count();
        eprintln!(
            "Morpheme roles: {} suffixes, {} prefixes",
            suffix_count, prefix_count
        );
        (min, max, roles)
    } else {
        (0, 0, Vec::new())
    };

    eprintln!("Parsing connection matrix from {input_txt}...");
    let matrix = die!(
        ConnectionMatrix::from_text_with_roles(&text, fw_min, fw_max, roles),
        "Error parsing connection matrix: {}"
    );

    eprintln!("  Matrix size: {}x{}", matrix.num_ids(), matrix.num_ids());

    die!(
        matrix.save(Path::new(output_file)),
        "Error writing {output_file}: {}"
    );

    let file_size = fs::metadata(output_file).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "Wrote {output_file} ({:.1} MB)",
        file_size as f64 / 1_048_576.0
    );
}

pub fn info(file: &str) {
    let magic = fs::read(file)
        .ok()
        .and_then(|b| b.get(..4).map(|s| s.to_vec()));

    match magic.as_deref() {
        Some(b"LXCX") => info_conn(file),
        Some(b"LXDX") => info_dict(file),
        Some(other) => {
            eprintln!(
                "Unknown file format (magic: {:?})",
                String::from_utf8_lossy(other)
            );
            process::exit(1);
        }
        None => {
            eprintln!("Error reading file: {file}");
            process::exit(1);
        }
    }
}

fn info_dict(dict_file: &str) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );

    let file_size = fs::metadata(dict_file).map(|m| m.len()).unwrap_or(0);
    let (reading_count, entry_count) = dict.stats();

    println!("Dictionary: {dict_file}");
    println!("File size:  {:.1} MB", file_size as f64 / 1_048_576.0);
    println!("Readings:   {reading_count}");
    println!("Entries:    {entry_count}");

    let sample_keys = ["かんじ", "にほん", "とうきょう", "たべる"];
    println!();
    println!("Sample lookups:");
    for key in &sample_keys {
        let entries = dict.lookup(key);
        if !entries.is_empty() {
            let surfaces: Vec<&str> = entries.iter().take(5).map(|e| e.surface.as_str()).collect();
            println!("  {key} → {}", surfaces.join(", "));
        } else {
            println!("  {key} → (not found)");
        }
    }
}

fn info_conn(conn_file: &str) {
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );

    let file_size = fs::metadata(conn_file).map(|m| m.len()).unwrap_or(0);
    let num_ids = conn.num_ids();

    println!("Connection matrix: {conn_file}");
    println!("File size:  {:.1} MB", file_size as f64 / 1_048_576.0);
    println!("POS IDs:    {num_ids}");
    println!(
        "Matrix:     {num_ids}x{num_ids} = {} entries",
        num_ids as u64 * num_ids as u64
    );

    let fw_min = conn.fw_min();
    let fw_max = conn.fw_max();
    if fw_min != 0 {
        let fw_count = fw_max - fw_min + 1;
        println!("FW range:   {fw_min}..={fw_max} ({fw_count} IDs)");
    } else {
        println!("FW range:   (none)");
    }

    let mut role_counts = [0u32; 4];
    for id in 0..num_ids {
        let r = conn.role(id) as usize;
        if r < role_counts.len() {
            role_counts[r] += 1;
        }
    }
    println!(
        "Roles:      CW={}, FW={}, Suffix={}, Prefix={}",
        role_counts[0], role_counts[1], role_counts[2], role_counts[3]
    );
}

/// Merge B into A (cheapest wins per (reading, surface)). Symbols stay
/// outside that rule: set aside from both inputs and added back through
/// `add_symbols`, so the result keeps the invariant `compile` builds. A
/// symbol of B whose (reading, surface) is also among A's symbols comes from
/// identical source rows (both were compiled with the same symbols), not a
/// conflict: it is dropped before `add_symbols`. A symbol that collides with
/// a lattice word stays an error.
fn merge_entries(merged: &mut EntryMap, mut b_entries: EntryMap) -> Result<(), String> {
    let symbols_a = take_symbols(merged);
    let mut symbols_b = take_symbols(&mut b_entries);
    for (reading, list) in symbols_b.iter_mut() {
        if let Some(a_list) = symbols_a.get(reading) {
            list.retain(|e| !a_list.iter().any(|a| a.surface == e.surface));
        }
    }
    symbols_b.retain(|_, list| !list.is_empty());

    for (reading, entries) in b_entries {
        let slot = merged.entry(reading).or_default();
        for entry in entries {
            if let Some(existing) = slot.iter_mut().find(|e| e.surface == entry.surface) {
                if entry.cost < existing.cost {
                    *existing = entry;
                }
            } else {
                slot.push(entry);
            }
        }
    }

    for symbols in [symbols_a, symbols_b] {
        add_symbols(merged, symbols)?;
    }
    Ok(())
}

pub struct MergeOptions {
    pub max_cost: Option<i16>,
    pub max_reading_len: Option<usize>,
}

pub fn merge(dict_a_file: &str, dict_b_file: &str, output_file: &str, opts: &MergeOptions) {
    eprintln!("Loading {dict_a_file}...");
    let dict_a = die!(
        TrieDictionary::open(Path::new(dict_a_file)),
        "Error opening dictionary A: {}"
    );
    let (a_readings, a_entries) = dict_a.stats();
    eprintln!("  A: {a_readings} readings, {a_entries} entries");

    eprintln!("Loading {dict_b_file}...");
    let dict_b = die!(
        TrieDictionary::open(Path::new(dict_b_file)),
        "Error opening dictionary B: {}"
    );
    let (b_readings, b_entries) = dict_b.stats();
    eprintln!("  B: {b_readings} readings, {b_entries} entries");

    eprintln!("Merging...");
    let mut merged: EntryMap = dict_a.iter().collect();
    die!(
        merge_entries(&mut merged, dict_b.iter().collect()),
        "Error merging symbols: {}"
    );

    let pre_filter_readings = merged.len();
    let pre_filter_entries: usize = merged.values().map(|v| v.len()).sum();

    if let Some(max_len) = opts.max_reading_len {
        merged.retain(|reading, _| reading.chars().count() <= max_len);
    }
    if let Some(max_cost) = opts.max_cost {
        for entries in merged.values_mut() {
            entries.retain(|e| e.cost <= max_cost);
        }
        merged.retain(|_, entries| !entries.is_empty());
    }

    let reading_count = merged.len();
    let entry_count: usize = merged.values().map(|v| v.len()).sum();

    if opts.max_cost.is_some() || opts.max_reading_len.is_some() {
        let dropped_readings = pre_filter_readings - reading_count;
        let dropped_entries = pre_filter_entries - entry_count;
        eprintln!("Filtered: dropped {dropped_readings} readings, {dropped_entries} entries");
    }

    eprintln!("Building trie from {reading_count} readings ({entry_count} entries)...");

    let dict = TrieDictionary::from_entries(merged);
    die!(
        dict.save(Path::new(output_file)),
        "Error writing dictionary: {}"
    );

    let file_size = fs::metadata(output_file).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "Wrote {output_file} ({:.1} MB)",
        file_size as f64 / 1_048_576.0
    );
}

fn collect_pairs(dict: &TrieDictionary) -> (HashSet<(String, String)>, HashSet<String>) {
    let mut pairs = HashSet::new();
    let mut readings = HashSet::new();
    for (reading, entries) in dict.iter() {
        for entry in &entries {
            pairs.insert((reading.clone(), entry.surface.clone()));
        }
        readings.insert(reading);
    }
    (pairs, readings)
}

fn first_entry_by_reading(dict: &TrieDictionary) -> HashMap<String, DictEntry> {
    let mut map = HashMap::new();
    for (reading, mut entries) in dict.iter() {
        if !entries.is_empty() {
            map.insert(reading, entries.swap_remove(0));
        }
    }
    map
}

pub fn diff(dict_a_file: &str, dict_b_file: &str) {
    eprintln!("Loading {dict_a_file}...");
    let dict_a = die!(
        TrieDictionary::open(Path::new(dict_a_file)),
        "Error opening dictionary A: {}"
    );
    eprintln!("Loading {dict_b_file}...");
    let dict_b = die!(
        TrieDictionary::open(Path::new(dict_b_file)),
        "Error opening dictionary B: {}"
    );

    let (a_readings, a_entries) = dict_a.stats();
    let (b_readings, b_entries) = dict_b.stats();

    eprintln!("Collecting pairs...");
    let (pairs_a, readings_a) = collect_pairs(&dict_a);
    let (pairs_b, readings_b) = collect_pairs(&dict_b);

    let readings_only_a = readings_a.difference(&readings_b).count();
    let readings_only_b = readings_b.difference(&readings_a).count();
    let readings_both = readings_a.intersection(&readings_b).count();

    let pairs_only_a = pairs_a.difference(&pairs_b).count();
    let pairs_only_b = pairs_b.difference(&pairs_a).count();
    let pairs_both = pairs_a.intersection(&pairs_b).count();

    println!("=== Dictionary Diff ===");
    println!("A: {dict_a_file} ({a_readings} readings, {a_entries} entries)");
    println!("B: {dict_b_file} ({b_readings} readings, {b_entries} entries)");
    println!();
    println!("Readings only in A: {readings_only_a:>10}");
    println!("Readings only in B: {readings_only_b:>10}");
    println!("Readings in both:   {readings_both:>10}");
    println!();
    println!("Surface pairs (reading+surface):");
    println!("  Only in A: {pairs_only_a:>10}");
    println!("  Only in B: {pairs_only_b:>10}");
    println!("  In both:   {pairs_both:>10}");

    let sample_readings: Vec<&String> = readings_b.difference(&readings_a).take(20).collect();
    if !sample_readings.is_empty() {
        let b_first = first_entry_by_reading(&dict_b);
        println!();
        println!("--- Sample: readings only in B (up to 20) ---");
        for reading in &sample_readings {
            if let Some(entry) = b_first.get(*reading) {
                println!("  {} -> {} (cost={})", reading, entry.surface, entry.cost);
            }
        }
    }

    let sample_readings_a: Vec<&String> = readings_a.difference(&readings_b).take(20).collect();
    if !sample_readings_a.is_empty() {
        let a_first = first_entry_by_reading(&dict_a);
        println!();
        println!("--- Sample: readings only in A (up to 20) ---");
        for reading in &sample_readings_a {
            if let Some(entry) = a_first.get(*reading) {
                println!("  {} -> {} (cost={})", reading, entry.surface, entry.cost);
            }
        }
    }
}

fn print_entries(entries: &[DictEntry]) {
    for e in entries {
        println!(
            "  {} \tcost={}\tL={}\tR={}",
            e.surface, e.cost, e.left_id, e.right_id
        );
    }
}

pub fn lookup(dict_file: &str, reading: &str) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let entries = dict.lookup(reading);
    if entries.is_empty() {
        println!("{reading}: not found");
    } else {
        println!("{reading}: {} entries", entries.len());
        print_entries(&entries);
    }
}

pub fn prefix(dict_file: &str, query: &str) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let results = dict.common_prefix_search(query);
    if results.is_empty() {
        println!("{query}: no prefix matches");
        return;
    }
    for r in &results {
        println!("{} ({} entries):", r.reading, r.entries.len());
        print_entries(&r.entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict_source::from_name;

    /// Every tag the curated sources allow must be in id.def (they resolve
    /// their whole allowed set), so the fixture lists them all.
    const ID_DEF: &str = "0 BOS/EOS,*,*,*,*,*,*
1 名詞,一般,*,*,*,*,*
2 記号,一般,*,*,*,*,*
3 記号,アルファベット,*,*,*,*,*
4 名詞,固有名詞,一般,*,*,*,*
5 名詞,固有名詞,人名,一般,*,*,*
6 名詞,固有名詞,人名,名,*,*,*
7 名詞,固有名詞,組織,*,*,*,*
8 名詞,固有名詞,地域,一般,*,*,*
9 名詞,固有名詞,地域,国,*,*,*
";

    /// Synthetic mozc / symbols / extras input dirs under a fresh temp dir.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        /// `mozc` rows are `reading\tsurface\tcost` (POS id 2 = 記号,一般),
        /// `symbols` rows `reading\tsurface` (written as 記号,一般 — the same
        /// POS as the mozc rows: the symbol flag comes from the source, so
        /// nothing may tell the two apart by POS), `extras` rows
        /// `reading\tsurface\tcost` (written as 名詞,一般).
        fn new(name: &str, mozc: &[&str], symbols: &[&str], extras: &[&str]) -> Self {
            let root = std::env::temp_dir()
                .join(format!("lexime_test_compile_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            for d in ["mozc", "symbols", "extras"] {
                fs::create_dir_all(root.join(d)).unwrap();
            }
            let to_mozc = |row: &&str, id: u16| {
                let f: Vec<&str> = row.split('\t').collect();
                format!("{}\t{id}\t{id}\t{}\t{}\n", f[0], f[2], f[1])
            };
            let mozc_txt: String = mozc.iter().map(|r| to_mozc(r, 2)).collect();
            let symbols_txt: String = symbols
                .iter()
                .map(|r| format!("{r}\t記号,一般,*,*,*,*,*\n"))
                .collect();
            let extras_txt: String = extras
                .iter()
                .map(|r| {
                    let (word, cost) = r.rsplit_once('\t').unwrap();
                    format!("{word}\t名詞,一般,*,*,*,*,*\t{cost}\n")
                })
                .collect();
            fs::write(root.join("mozc/dictionary00.txt"), mozc_txt).unwrap();
            fs::write(root.join("mozc/id.def"), ID_DEF).unwrap();
            fs::write(root.join("symbols/symbols.tsv"), symbols_txt).unwrap();
            fs::write(root.join("extras/it.tsv"), extras_txt).unwrap();
            Self { root }
        }

        fn dir(&self, d: &str) -> String {
            self.root.join(d).to_string_lossy().into_owned()
        }

        fn build(&self, symbols_first: bool) -> Result<EntryMap, String> {
            let symbols = ("symbols".to_string(), self.dir("symbols"));
            let extras = ("extras".to_string(), self.dir("extras"));
            let order = if symbols_first {
                [symbols, extras]
            } else {
                [extras, symbols]
            };
            build_entries("mozc", &self.dir("mozc"), None, &order)
        }

        fn dict(&self, symbols_first: bool) -> TrieDictionary {
            TrieDictionary::from_entries(self.build(symbols_first).unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn assert_duplicate_err(fx: &Fixture, pair: &str) {
        for symbols_first in [true, false] {
            let err = fx.build(symbols_first).unwrap_err();
            assert!(
                err.contains(pair),
                "symbols_first={symbols_first}: error must name {pair}: {err}"
            );
        }
    }

    /// a symbols row equal to a mozc or an extras (reading, surface) is
    /// a build error naming the pair, not a silent cheapest-wins merge.
    #[test]
    fn symbol_duplicating_lattice_source_is_err() {
        let fx = Fixture::new(
            "dup_mozc",
            &["ぱい\tπ\t4898", "ぱい\tパイ\t3000"],
            &["ぱい\tπ", "ぱい\tΠ"],
            &[],
        );
        assert_duplicate_err(&fx, "(ぱい, π)");

        let fx = Fixture::new(
            "dup_extras",
            &["ほし\t星\t3000"],
            &["ほし\t★"],
            &["ほし\t★\t9000"],
        );
        assert_duplicate_err(&fx, "(ほし, ★)");
    }

    /// the duplicate is an error whether the symbol's
    /// compiled cost (its row index) is below or above the Mozc entry's.
    #[test]
    fn symbol_duplicate_err_regardless_of_cost() {
        // symbol cost 0 < mozc 5000
        let fx = Fixture::new("dup_lower", &["ぱい\tπ\t5000"], &["ぱい\tπ"], &[]);
        assert_duplicate_err(&fx, "(ぱい, π)");
        // symbol cost 0 > mozc -500
        let fx = Fixture::new("dup_higher", &["ぱい\tπ\t-500"], &["ぱい\tπ"], &[]);
        assert_duplicate_err(&fx, "(ぱい, π)");
    }

    /// swapping `--extra-source` order yields the same dictionary bytes.
    /// The symbol ★ (cost 0) ties the extras word 干し (cost 0) on ほし, so
    /// merging symbols in CLI order would change the entry order.
    #[test]
    fn extra_source_order_does_not_change_dict() {
        let fx = Fixture::new(
            "order",
            &["ほし\t星\t3000", "いち\t一\t2000"],
            &["ほし\t★", "ほし\t☆", "まる\t○"],
            &["ほし\t干し\t0", "いち\t壱\t1000"],
        );
        let a = fx.dict(true).to_bytes().unwrap();
        let b = fx.dict(false).to_bytes().unwrap();
        assert!(a == b, "dict bytes depend on --extra-source order");
    }

    /// a symbol's cost is its row index among same-reading rows, and
    /// `lookup` returns them in row order.
    #[test]
    fn symbol_cost_is_row_order() {
        let fx = Fixture::new(
            "row_order",
            &["しかく\t四角\t3000"],
            &["ほし\t★", "ほし\t☆", "しかく\t□", "ほし\t✪"],
            &[],
        );
        let dict = fx.dict(true);
        let hoshi: Vec<(String, i16)> = dict
            .lookup("ほし")
            .into_iter()
            .map(|e| (e.surface, e.cost))
            .collect();
        assert_eq!(
            hoshi,
            vec![
                ("★".to_string(), 0),
                ("☆".to_string(), 1),
                ("✪".to_string(), 2)
            ]
        );
    }

    /// Two symbol sources (compile) or both dictionaries' symbols (merge)
    /// entering through `add_symbols` for the same reading number 0..n in
    /// arrival order: A's rows, then B's.
    #[test]
    fn symbols_from_two_sources_number_in_arrival_order() {
        let sym = |surface: &str| DictEntry {
            surface: surface.to_string(),
            cost: 0,
            left_id: 2,
            right_id: 2,
            symbol: true,
        };
        let batch = |surfaces: &[&str]| -> EntryMap {
            EntryMap::from([(
                "ほし".to_string(),
                surfaces.iter().map(|s| sym(s)).collect(),
            )])
        };
        let mut entries = EntryMap::new();
        assert_eq!(add_symbols(&mut entries, batch(&["★", "☆"])), Ok(2));
        assert_eq!(add_symbols(&mut entries, batch(&["✪", "✫"])), Ok(2));
        let dict = TrieDictionary::from_entries(entries);
        let got: Vec<(String, i16)> = dict
            .lookup("ほし")
            .into_iter()
            .map(|e| (e.surface, e.cost))
            .collect();
        let want: Vec<(String, i16)> = ["★", "☆", "✪", "✫"]
            .iter()
            .enumerate()
            .map(|(i, s)| (s.to_string(), i as i16))
            .collect();
        assert_eq!(got, want);
    }

    /// Merging two dictionaries compiled with the same symbols keeps each
    /// symbol once; a symbol colliding with a lattice word is still an Err.
    #[test]
    fn merge_entries_dedups_shared_symbols_and_rejects_word_collisions() {
        let mk = |surface: &str, symbol: bool| DictEntry {
            surface: surface.to_string(),
            cost: 0,
            left_id: 2,
            right_id: 2,
            symbol,
        };
        let dict = |rows: &[(&str, bool)]| -> EntryMap {
            EntryMap::from([(
                "ほし".to_string(),
                rows.iter().map(|(s, f)| mk(s, *f)).collect(),
            )])
        };
        let surfaces = |m: &EntryMap| -> Vec<(String, i16)> {
            m["ほし"]
                .iter()
                .map(|e| (e.surface.clone(), e.cost))
                .collect()
        };
        // Merge of a dictionary with itself.
        let mut a = dict(&[("星", false), ("★", true), ("☆", true)]);
        let copy = a.clone();
        merge_entries(&mut a, copy).unwrap();
        assert_eq!(
            surfaces(&a),
            [("星".into(), 0), ("★".into(), 0), ("☆".into(), 1)]
        );
        // Shared symbol once, B's own symbol follows in arrival order.
        let mut a = dict(&[("★", true)]);
        merge_entries(&mut a, dict(&[("★", true), ("✪", true)])).unwrap();
        assert_eq!(surfaces(&a), [("★".into(), 0), ("✪".into(), 1)]);
        // A symbol of B equal to a lattice word of A is still an Err.
        let mut a = dict(&[("★", false)]);
        let err = merge_entries(&mut a, dict(&[("★", true)])).unwrap_err();
        assert!(err.contains("(ほし, ★)"), "{err}");
    }

    /// Only the symbols source's entries carry the flag, and it survives the
    /// v5 file round-trip.
    #[test]
    fn only_symbols_rows_are_flagged() {
        let fx = Fixture::new(
            "flag",
            &["ぱい\tパイ\t3000"],
            &["ぱい\tΠ"],
            &["ぱい\t牌\t6000"],
        );
        let bytes = fx.dict(true).to_bytes().unwrap();
        let dict = TrieDictionary::from_bytes(&bytes).unwrap();
        let mut flags: Vec<(String, bool)> = dict
            .lookup("ぱい")
            .into_iter()
            .map(|e| (e.surface, e.symbol))
            .collect();
        flags.sort();
        assert_eq!(
            flags,
            vec![
                ("Π".to_string(), true),
                ("パイ".to_string(), false),
                ("牌".to_string(), false),
            ]
        );
    }

    /// the bundled symbols TSV over the pinned mozc-raw duplicates
    /// no (reading, surface) of it or of the bundled extras (the check `mise
    /// run dict`, the accuracy job's build, enforces) — 155 symbol entries, the largest symbol cost is the most same-reading rows − 1
    /// (cost = row index), and with no history the prediction stage lists no
    /// symbol for any proper prefix of a symbol reading. Needs
    /// `engine/data/mozc-raw`; run with `--ignored`.
    #[test]
    #[ignore]
    fn bundled_symbols_row_costs_and_no_untrained_prediction() {
        let Some(result) = build_bundled() else {
            return;
        };
        let entries = result.unwrap();
        let rows: Vec<(&String, usize, i16)> = entries
            .iter()
            .filter_map(|(r, es)| {
                let syms: Vec<&DictEntry> = es.iter().filter(|e| e.symbol).collect();
                let max_cost = syms.iter().map(|e| e.cost).max()?;
                Some((r, syms.len(), max_cost))
            })
            .collect();
        assert_eq!(rows.iter().map(|&(_, n, _)| n).sum::<usize>(), 155);
        let most_rows = rows.iter().map(|&(_, n, _)| n).max().unwrap();
        let max_cost = rows.iter().map(|&(_, _, c)| c).max().unwrap();
        assert_eq!(i64::from(max_cost), most_rows as i64 - 1);

        let readings: Vec<String> = rows.iter().map(|&(r, _, _)| r.clone()).collect();
        let dict = TrieDictionary::from_entries(entries);
        let ctx = lex_core::converter::ConversionContext::new(&dict, None);
        let mut prefixes = HashSet::new();
        for reading in &readings {
            let chars: Vec<char> = reading.chars().collect();
            for n in 1..chars.len() {
                prefixes.insert(chars[..n].iter().collect::<String>());
            }
        }
        let mut leaked = Vec::new();
        for prefix in &prefixes {
            let priced = lex_core::candidates::production_candidates_priced(&ctx, prefix);
            for p in priced.diagnostics.predictions {
                if dict
                    .lookup(&p.reading)
                    .iter()
                    .any(|e| e.symbol && e.surface == p.surface)
                {
                    leaked.push((prefix.clone(), p.reading, p.surface));
                }
            }
        }
        assert!(
            prefixes.len() > 50,
            "{} prefixes of symbol readings",
            prefixes.len()
        );
        assert!(leaked.is_empty(), "untrained symbols predicted: {leaked:?}");
    }

    /// The bundled extras rows whose cost is not 5000 (the cost-overrides).
    /// Written here, not read from the TSVs, so a changed cost in a TSV fails
    /// T9.
    const EXTRAS_COST_OVERRIDES: &[(&str, i16)] = &[
        ("あくゆう", 8000),
        ("そうそう", 8000),
        ("かんぽう", 7000),
        ("ましん", 3500),
    ];

    /// T9: compiled through `build_entries` with a pin-shaped id.def, every
    /// bundled extras row keeps its surface and its cost (5000, or the
    /// override) and gets the id of its tag — the person-name rows included:
    /// the role-based offsets (+2000 for 人名) belong to the Mozc import and
    /// are applied before the extras merge, never to a curated row. A Mozc
    /// 人名 row still gets the offset, and the two Mozc rows the bundled
    /// extras share (一保堂, マシン — as in the pin) are replaced by the
    /// cheaper extras row.
    #[test]
    fn t9_extras_cost_is_final_and_mozc_keeps_its_offset() {
        use crate::dict_source::test_support::{pin_id, pin_shaped_id_def, scratch};
        let root = scratch("t9_extras_cost");
        let mozc = root.join("mozc");
        fs::create_dir_all(&mozc).unwrap();
        let id_def = pin_shaped_id_def(&mozc);
        fs::write(
            mozc.join("dictionary00.txt"),
            "たなか\t1921\t1921\t5000\t田中\n\
             かんじ\t1851\t1851\t5100\t漢字\n\
             いっぽどう\t1922\t1922\t6898\t一保堂\n\
             ましん\t1851\t1851\t4241\tマシン\n",
        )
        .unwrap();
        let extras_dir = root.join("extras");
        from_name("extras").unwrap().fetch(&extras_dir).unwrap();
        let rows = crate::dict_source::bundled_extras_rows();
        let entries = build_entries(
            "mozc",
            &mozc.to_string_lossy(),
            Some(&id_def.to_string_lossy()),
            &[(
                "extras".to_string(),
                extras_dir.to_string_lossy().into_owned(),
            )],
        )
        .unwrap();
        fs::remove_dir_all(&root).ok();

        assert_eq!(rows.len(), 47);
        for row in &rows {
            let (reading, surface, tag) = (row[0], row[1], row[2]);
            let same: Vec<&DictEntry> = entries[reading]
                .iter()
                .filter(|e| e.surface == surface)
                .collect();
            assert_eq!(same.len(), 1, "{row:?}");
            let want_cost = EXTRAS_COST_OVERRIDES
                .iter()
                .find(|(r, _)| *r == reading)
                .map_or(5000, |&(_, c)| c);
            let want_id = pin_id(tag);
            assert_eq!(
                (same[0].left_id, same[0].right_id, same[0].cost),
                (want_id, want_id, want_cost),
                "{row:?}"
            );
            assert!(!same[0].symbol);
        }
        let person = rows.iter().filter(|r| r[2].contains("人名")).count();
        assert_eq!(person, 11);

        let mozc_entry = |reading: &str| {
            let list = &entries[reading];
            assert_eq!(list.len(), 1, "{list:?}");
            (list[0].left_id, list[0].cost)
        };
        assert_eq!(mozc_entry("たなか"), (1921, 5000 + PERSON_NAME_COST_OFFSET));
        assert_eq!(mozc_entry("かんじ"), (1851, 5100));
        assert_eq!(mozc_entry("いっぽどう"), (1929, 5000));
        assert_eq!(mozc_entry("ましん"), (1851, 3500));
    }

    /// `build_entries` over the pinned mozc-raw and the bundled symbols and
    /// extras; `None` (skipped) when mozc-raw is not fetched.
    fn build_bundled() -> Option<Result<EntryMap, String>> {
        let mozc_raw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mozc-raw");
        if !mozc_raw.join("id.def").exists() {
            eprintln!("skipping: mozc-raw not found at {mozc_raw:?}");
            return None;
        }
        let root = std::env::temp_dir().join(format!(
            "lexime_test_bundled_symbols_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut extra_sources = Vec::new();
        for name in ["symbols", "extras"] {
            let dir = root.join(name);
            from_name(name).unwrap().fetch(&dir).unwrap();
            extra_sources.push((name.to_string(), dir.to_string_lossy().into_owned()));
        }
        let id_def = mozc_raw.join("id.def");
        let result = build_entries(
            "mozc",
            &mozc_raw.to_string_lossy(),
            Some(&id_def.to_string_lossy()),
            &extra_sources,
        );
        fs::remove_dir_all(&root).ok();
        Some(result)
    }
}
