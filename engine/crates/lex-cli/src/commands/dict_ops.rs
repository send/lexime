use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::dict_source::{self, pos_map, AVAILABLE_SOURCES};
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

pub fn fetch(source_name: &str, output_dir: &str) {
    let output_dir = Path::new(output_dir);
    let dict_source = dict_source::from_name(source_name).unwrap_or_else(|| {
        eprintln!("Error: unknown source '{source_name}' (available: {AVAILABLE_SOURCES})");
        process::exit(1);
    });
    die!(
        dict_source.fetch(output_dir),
        "Error fetching dictionary: {}"
    );
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
    let dict_source = dict_source::from_name(source_name).unwrap_or_else(|| {
        eprintln!("Error: unknown source '{source_name}' (available: {AVAILABLE_SOURCES})");
        process::exit(1);
    });

    let input_path = Path::new(input_dir);
    if !input_path.is_dir() {
        eprintln!("Error: {input_dir} is not a directory");
        process::exit(1);
    }

    eprintln!("Source: {source_name}");
    let mut entries = die!(
        dict_source.parse_dir(input_path),
        "Error parsing dictionary: {}"
    );

    // Parse extra sources up front, but defer merging until after id.def
    // adjustments below — extras carry hand-tuned costs that must not be
    // rewritten by role-based offsets.
    let mut extras: Vec<(String, HashMap<String, Vec<DictEntry>>)> = Vec::new();
    for (extra_name, extra_dir) in extra_sources {
        let extra_src = dict_source::from_name(extra_name).unwrap_or_else(|| {
            eprintln!(
                "Error: unknown extra source '{extra_name}' (available: {AVAILABLE_SOURCES})"
            );
            process::exit(1);
        });
        let extra_path = Path::new(extra_dir);
        if !extra_path.is_dir() {
            eprintln!("Error: extra input {extra_dir} is not a directory");
            process::exit(1);
        }
        eprintln!("Extra source: {extra_name}");
        let parsed = die!(
            extra_src.parse_dir(extra_path),
            "Error parsing extra dictionary: {}"
        );
        extras.push((extra_name.clone(), parsed));
    }

    // Apply compile-time cost offsets based on morpheme roles.
    // Auto-detect id.def in input_dir if --id-def is not specified.
    let id_def_path = id_def.map(PathBuf::from).or_else(|| {
        let auto = input_path.join("id.def");
        if auto.is_file() {
            eprintln!("Auto-detected id.def at {}", auto.display());
            Some(auto)
        } else {
            None
        }
    });
    if let Some(id_def_path) = &id_def_path {
        let roles = die!(
            pos_map::morpheme_roles(id_def_path),
            "Error loading morpheme roles: {}"
        );
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
        for (reading, list) in extra_entries {
            let slot = entries.entry(reading).or_default();
            if slot.is_empty() {
                added_readings += 1;
            }
            for entry in list {
                if !slot.iter().any(|e| {
                    e.surface == entry.surface
                        && e.left_id == entry.left_id
                        && e.right_id == entry.right_id
                }) {
                    slot.push(entry);
                    added_entries += 1;
                }
            }
        }
        eprintln!("Merged '{extra_name}': +{added_readings} readings, +{added_entries} entries");
    }

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
    let mut merged: HashMap<String, Vec<DictEntry>> = HashMap::new();

    for (reading, entries) in dict_a.iter() {
        merged.entry(reading).or_default().extend(entries);
    }

    for (reading, entries) in dict_b.iter() {
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
