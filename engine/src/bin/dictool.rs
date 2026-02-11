use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process;

use lex_engine::dict::connection::ConnectionMatrix;
use lex_engine::dict::source;
use lex_engine::dict::source::SudachiSource;
use lex_engine::dict::{DictEntry, Dictionary, TrieDictionary};

use lex_engine::dict::source::pos_map;

/// Unwrap a Result or print the error and exit.
macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
    }

    match args[1].as_str() {
        "compile" => parse_compile(&args[2..]),
        "compile-conn" => {
            if args.len() != 4 {
                eprintln!("Usage: dictool compile-conn <input-txt> <output-file>");
                process::exit(1);
            }
            compile_conn(&args[2], &args[3]);
        }
        "diff" => {
            if args.len() != 4 {
                eprintln!("Usage: dictool diff <dict-a> <dict-b>");
                process::exit(1);
            }
            diff(&args[2], &args[3]);
        }
        "merge" => parse_merge(&args[2..]),
        "fetch" => parse_fetch(&args[2..]),
        "info" => {
            if args.len() != 3 {
                eprintln!("Usage: dictool info <dict-file>");
                process::exit(1);
            }
            info(&args[2]);
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!("Usage: dictool <command>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  fetch         [--source mozc|sudachi] [--full] <output-dir>");
    eprintln!(
        "  compile       [--source mozc|sudachi] [--remap-ids <id.def>] <input-dir> <output-file>"
    );
    eprintln!("  compile-conn  <input-txt> <output-file>");
    eprintln!("  info          <dict-file>");
    eprintln!(
        "  merge         [--max-cost N] [--max-reading-len N] <dict-a> <dict-b> <output-file>"
    );
    eprintln!("  diff          <dict-a> <dict-b>");
    process::exit(1);
}

/// Parsed flags from argument parsing.
struct ParsedArgs<'a> {
    source_name: &'a str,
    full: bool,
    remap_ids: Option<&'a str>,
    positional: Vec<&'a str>,
}

/// Parse `[--source mozc|sudachi] [--full] [--remap-ids <path>] <positional>...`.
fn parse_source_args(args: &[String]) -> ParsedArgs<'_> {
    let mut source_name = "mozc";
    let mut full = false;
    let mut remap_ids = None;
    let mut positional = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--source" {
            i += 1;
            if i >= args.len() {
                eprintln!("Error: --source requires a value (mozc, sudachi)");
                process::exit(1);
            }
            source_name = args[i].as_str();
        } else if args[i] == "--remap-ids" {
            i += 1;
            if i >= args.len() {
                eprintln!("Error: --remap-ids requires a path to Mozc id.def");
                process::exit(1);
            }
            remap_ids = Some(args[i].as_str());
        } else if args[i] == "--full" {
            full = true;
        } else {
            positional.push(args[i].as_str());
        }
        i += 1;
    }

    ParsedArgs {
        source_name,
        full,
        remap_ids,
        positional,
    }
}

fn parse_compile(args: &[String]) {
    let parsed = parse_source_args(args);
    if parsed.positional.len() != 2 {
        eprintln!("Usage: dictool compile [--source mozc|sudachi] [--remap-ids <id.def>] <input-dir> <output-file>");
        process::exit(1);
    }
    compile(
        parsed.source_name,
        parsed.remap_ids,
        parsed.positional[0],
        parsed.positional[1],
    );
}

fn parse_fetch(args: &[String]) {
    let parsed = parse_source_args(args);
    if parsed.positional.len() != 1 {
        eprintln!("Usage: dictool fetch [--source mozc|sudachi] [--full] <output-dir>");
        process::exit(1);
    }

    let output_dir = Path::new(parsed.positional[0]);

    if parsed.full {
        if parsed.source_name != "sudachi" {
            eprintln!("Error: --full is only supported for sudachi source");
            process::exit(1);
        }
        let source = SudachiSource;
        die!(
            source.fetch_full(output_dir),
            "Error fetching dictionary: {}"
        );
    } else {
        let dict_source = source::from_name(parsed.source_name).unwrap_or_else(|| {
            eprintln!(
                "Error: unknown source '{}' (available: mozc, sudachi)",
                parsed.source_name
            );
            process::exit(1);
        });
        die!(
            dict_source.fetch(output_dir),
            "Error fetching dictionary: {}"
        );
    }
}

fn compile(source_name: &str, remap_ids: Option<&str>, input_dir: &str, output_file: &str) {
    let dict_source = source::from_name(source_name).unwrap_or_else(|| {
        eprintln!("Error: unknown source '{source_name}' (available: mozc, sudachi)");
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

    // Apply POS ID remapping if --remap-ids is specified
    if let Some(id_def_path) = remap_ids {
        eprintln!("Remapping POS IDs using {id_def_path}...");
        let mozc_ids = die!(
            pos_map::parse_mozc_id_def(Path::new(id_def_path)),
            "Error parsing id.def: {}"
        );
        eprintln!("  Loaded {} generic Mozc POS entries", mozc_ids.len());

        let (remap, matched, total) = die!(
            pos_map::build_remap_table(input_path, &mozc_ids),
            "Error building remap table: {}"
        );
        eprintln!("  Remapped {matched} of {total} unique Sudachi IDs");

        pos_map::remap_entries(&mut entries, &remap);
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

fn compile_conn(input_txt: &str, output_file: &str) {
    let text = die!(
        fs::read_to_string(input_txt),
        "Error reading {input_txt}: {}"
    );

    eprintln!("Parsing connection matrix from {input_txt}...");
    let matrix = die!(
        ConnectionMatrix::from_text(&text),
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

fn info(dict_file: &str) {
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

    // Sample some entries
    let sample_keys = ["かんじ", "にほん", "とうきょう", "たべる"];
    println!();
    println!("Sample lookups:");
    for key in &sample_keys {
        if let Some(entries) = dict.lookup(key) {
            let surfaces: Vec<&str> = entries.iter().take(5).map(|e| e.surface.as_str()).collect();
            println!("  {key} → {}", surfaces.join(", "));
        } else {
            println!("  {key} → (not found)");
        }
    }
}

struct MergeOptions {
    max_cost: Option<i16>,
    max_reading_len: Option<usize>,
}

fn parse_merge(args: &[String]) {
    let mut max_cost: Option<i16> = None;
    let mut max_reading_len: Option<usize> = None;
    let mut positional = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--max-cost" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --max-cost requires a value");
                    process::exit(1);
                }
                max_cost = Some(args[i].parse().unwrap_or_else(|_| {
                    eprintln!("Error: invalid --max-cost value: {}", args[i]);
                    process::exit(1);
                }));
            }
            "--max-reading-len" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --max-reading-len requires a value");
                    process::exit(1);
                }
                max_reading_len = Some(args[i].parse().unwrap_or_else(|_| {
                    eprintln!("Error: invalid --max-reading-len value: {}", args[i]);
                    process::exit(1);
                }));
            }
            _ => positional.push(args[i].as_str()),
        }
        i += 1;
    }

    if positional.len() != 3 {
        eprintln!(
            "Usage: dictool merge [--max-cost N] [--max-reading-len N] <dict-a> <dict-b> <output-file>"
        );
        process::exit(1);
    }

    let opts = MergeOptions {
        max_cost,
        max_reading_len,
    };
    merge(positional[0], positional[1], positional[2], &opts);
}

fn merge(dict_a_file: &str, dict_b_file: &str, output_file: &str, opts: &MergeOptions) {
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

    // Insert all entries from A.
    for (reading, entries) in dict_a.iter() {
        merged
            .entry(reading)
            .or_default()
            .extend(entries.iter().cloned());
    }

    // Insert entries from B, deduplicating by surface and keeping lower cost.
    for (reading, entries) in dict_b.iter() {
        let slot = merged.entry(reading).or_default();
        for entry in entries {
            if let Some(existing) = slot.iter_mut().find(|e| e.surface == entry.surface) {
                if entry.cost < existing.cost {
                    *existing = entry.clone();
                }
            } else {
                slot.push(entry.clone());
            }
        }
    }

    // Apply filters.
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

/// Collect all (reading, surface) pairs and the set of readings from a dictionary.
fn collect_pairs(dict: &TrieDictionary) -> (HashSet<(String, String)>, HashSet<String>) {
    let mut pairs = HashSet::new();
    let mut readings = HashSet::new();
    for (reading, entries) in dict.iter() {
        readings.insert(reading.clone());
        for entry in entries {
            pairs.insert((reading.clone(), entry.surface.clone()));
        }
    }
    (pairs, readings)
}

/// Build a map from reading to first entry (for sampling).
fn first_entry_by_reading(dict: &TrieDictionary) -> std::collections::HashMap<String, DictEntry> {
    let mut map = std::collections::HashMap::new();
    for (reading, entries) in dict.iter() {
        if let Some(entry) = entries.first() {
            map.insert(reading, entry.clone());
        }
    }
    map
}

fn diff(dict_a_file: &str, dict_b_file: &str) {
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

    // Sample: readings only in B
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

    // Sample: readings only in A
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
