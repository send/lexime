use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process;

use clap::{Parser, Subcommand};

use lex_engine::converter::explain;
use lex_engine::converter::{
    convert, convert_nbest, convert_nbest_with_history, convert_with_history,
};
use lex_engine::dict::connection::ConnectionMatrix;
use lex_engine::dict::source;
use lex_engine::dict::source::SudachiSource;
use lex_engine::dict::{DictEntry, Dictionary, TrieDictionary};
use lex_engine::user_history::UserHistory;

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

#[derive(Parser)]
#[command(name = "dictool", about = "Lexime dictionary build tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download raw dictionary files
    Fetch {
        /// Dictionary source
        #[arg(long, default_value = "mozc")]
        source: String,
        /// Fetch full dictionary (sudachi only)
        #[arg(long)]
        full: bool,
        /// Output directory
        output_dir: String,
    },
    /// Compile dictionary from raw files
    Compile {
        /// Dictionary source
        #[arg(long, default_value = "mozc")]
        source: String,
        /// Remap POS IDs using Mozc id.def
        #[arg(long)]
        remap_ids: Option<String>,
        /// Input directory
        input_dir: String,
        /// Output file
        output_file: String,
    },
    /// Compile connection matrix
    CompileConn {
        /// Input text file
        input_txt: String,
        /// Output binary file
        output_file: String,
        /// Mozc id.def for function-word range extraction
        #[arg(long)]
        id_def: Option<String>,
    },
    /// Show dictionary info
    Info {
        /// Dictionary file
        dict_file: String,
    },
    /// Merge two dictionaries
    Merge {
        /// Maximum cost to keep
        #[arg(long)]
        max_cost: Option<i16>,
        /// Maximum reading length (in characters)
        #[arg(long)]
        max_reading_len: Option<usize>,
        /// First dictionary
        dict_a: String,
        /// Second dictionary
        dict_b: String,
        /// Output file
        output_file: String,
    },
    /// Show diff between two dictionaries
    Diff {
        /// First dictionary
        dict_a: String,
        /// Second dictionary
        dict_b: String,
    },
    /// Look up a reading in the dictionary (exact match)
    Lookup {
        /// Dictionary file
        dict_file: String,
        /// Reading to look up (hiragana)
        reading: String,
    },
    /// Common-prefix search (all readings that are prefixes of the query)
    Prefix {
        /// Dictionary file
        dict_file: String,
        /// Query string (hiragana)
        query: String,
    },
    /// Convert kana to kanji (N-best)
    Convert {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// Kana input
        kana: String,
        /// Number of candidates
        #[arg(short, long, default_value = "10")]
        n: usize,
        /// User history file (optional)
        #[arg(long)]
        history: Option<String>,
    },
    /// Generate conversion snapshot for regression testing
    Snapshot {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// Input file: one reading per line (hiragana)
        input_file: String,
        /// Output file (JSONL format)
        output_file: String,
        /// Number of candidates per reading
        #[arg(short, long, default_value = "5")]
        n: usize,
        /// User history file (optional)
        #[arg(long)]
        history: Option<String>,
    },
    /// Compare conversion results against a snapshot
    DiffSnapshot {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// Input file: one reading per line (hiragana)
        input_file: String,
        /// Expected snapshot (JSONL from `snapshot` command)
        expected_file: String,
        /// Number of candidates per reading
        #[arg(short, long, default_value = "5")]
        n: usize,
        /// User history file (optional)
        #[arg(long)]
        history: Option<String>,
    },
    /// Explain conversion cost breakdown for a reading
    Explain {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// Kana input (reading)
        reading: String,
        /// Filter to paths containing this surface (optional)
        #[arg(long)]
        surface: Option<String>,
        /// Number of paths to show
        #[arg(short, long, default_value = "5")]
        n: usize,
        /// User history file (optional)
        #[arg(long)]
        history: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Fetch {
            source: source_name,
            full,
            output_dir,
        } => {
            let output_dir = Path::new(&output_dir);
            if full {
                if source_name != "sudachi" {
                    eprintln!("Error: --full is only supported for sudachi source");
                    process::exit(1);
                }
                let src = SudachiSource;
                die!(src.fetch_full(output_dir), "Error fetching dictionary: {}");
            } else {
                let dict_source = source::from_name(&source_name).unwrap_or_else(|| {
                    eprintln!("Error: unknown source '{source_name}' (available: mozc, sudachi)");
                    process::exit(1);
                });
                die!(
                    dict_source.fetch(output_dir),
                    "Error fetching dictionary: {}"
                );
            }
        }
        Command::Compile {
            source: source_name,
            remap_ids,
            input_dir,
            output_file,
        } => compile(&source_name, remap_ids.as_deref(), &input_dir, &output_file),
        Command::CompileConn {
            input_txt,
            output_file,
            id_def,
        } => compile_conn(&input_txt, &output_file, id_def.as_deref()),
        Command::Info { dict_file } => info(&dict_file),
        Command::Merge {
            max_cost,
            max_reading_len,
            dict_a,
            dict_b,
            output_file,
        } => {
            let opts = MergeOptions {
                max_cost,
                max_reading_len,
            };
            merge(&dict_a, &dict_b, &output_file, &opts);
        }
        Command::Diff { dict_a, dict_b } => diff(&dict_a, &dict_b),
        Command::Lookup { dict_file, reading } => lookup(&dict_file, &reading),
        Command::Prefix { dict_file, query } => prefix(&dict_file, &query),
        Command::Convert {
            dict_file,
            conn_file,
            kana,
            n,
            history,
        } => convert_cmd(&dict_file, &conn_file, &kana, n, history.as_deref()),
        Command::Snapshot {
            dict_file,
            conn_file,
            input_file,
            output_file,
            n,
            history,
        } => snapshot_cmd(
            &dict_file,
            &conn_file,
            &input_file,
            &output_file,
            n,
            history.as_deref(),
        ),
        Command::DiffSnapshot {
            dict_file,
            conn_file,
            input_file,
            expected_file,
            n,
            history,
        } => diff_snapshot_cmd(
            &dict_file,
            &conn_file,
            &input_file,
            &expected_file,
            n,
            history.as_deref(),
        ),
        Command::Explain {
            dict_file,
            conn_file,
            reading,
            surface,
            n,
            history,
        } => explain_cmd(
            &dict_file,
            &conn_file,
            &reading,
            surface.as_deref(),
            n,
            history.as_deref(),
        ),
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

fn compile_conn(input_txt: &str, output_file: &str, id_def: Option<&str>) {
    let text = die!(
        fs::read_to_string(input_txt),
        "Error reading {input_txt}: {}"
    );

    let (fw_min, fw_max) = if let Some(id_def_path) = id_def {
        let (min, max) = die!(
            pos_map::function_word_id_range(Path::new(id_def_path)),
            "Error extracting function-word range: {}"
        );
        eprintln!("Function-word ID range: {min}..={max}");
        (min, max)
    } else {
        (0, 0)
    };

    eprintln!("Parsing connection matrix from {input_txt}...");
    let matrix = die!(
        ConnectionMatrix::from_text_with_metadata(&text, fw_min, fw_max),
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

fn print_entries(entries: &[DictEntry]) {
    for e in entries {
        println!(
            "  {} \tcost={}\tL={}\tR={}",
            e.surface, e.cost, e.left_id, e.right_id
        );
    }
}

fn lookup(dict_file: &str, reading: &str) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    match dict.lookup(reading) {
        Some(entries) => {
            println!("{reading}: {} entries", entries.len());
            print_entries(entries);
        }
        None => println!("{reading}: not found"),
    }
}

fn prefix(dict_file: &str, query: &str) {
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
        print_entries(r.entries);
    }
}

/// A single snapshot entry: reading → list of conversion results (surfaces).
#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotEntry {
    reading: String,
    surfaces: Vec<String>,
}

fn snapshot_cmd(
    dict_file: &str,
    conn_file: &str,
    input_file: &str,
    output_file: &str,
    n: usize,
    history: Option<&str>,
) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );
    let user_history = history.map(|path| {
        die!(
            UserHistory::open(Path::new(path)),
            "Error opening history: {}"
        )
    });

    let readings = read_input_lines(input_file);
    let mut out = die!(
        fs::File::create(output_file).map(std::io::BufWriter::new),
        "Error creating output: {}"
    );

    let mut count = 0;
    for reading in &readings {
        let nbest = if let Some(ref h) = user_history {
            convert_nbest_with_history(&dict, Some(&conn), h, reading, n)
        } else {
            convert_nbest(&dict, Some(&conn), reading, n)
        };

        let surfaces: Vec<String> = nbest
            .iter()
            .map(|path| path.iter().map(|s| s.surface.as_str()).collect())
            .collect();

        let entry = SnapshotEntry {
            reading: reading.clone(),
            surfaces,
        };
        die!(
            serde_json::to_writer(&mut out, &entry),
            "Error writing JSON: {}"
        );
        die!(writeln!(out), "Error writing newline: {}");
        count += 1;
    }

    eprintln!("Wrote {count} entries to {output_file}");
}

fn diff_snapshot_cmd(
    dict_file: &str,
    conn_file: &str,
    input_file: &str,
    expected_file: &str,
    n: usize,
    history: Option<&str>,
) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );
    let user_history = history.map(|path| {
        die!(
            UserHistory::open(Path::new(path)),
            "Error opening history: {}"
        )
    });

    // Load expected results
    let expected: HashMap<String, Vec<String>> = {
        let file = die!(
            fs::File::open(expected_file),
            "Error opening expected file: {}"
        );
        let reader = BufReader::new(file);
        let mut map = HashMap::new();
        for line in reader.lines() {
            let line = die!(line, "Error reading line: {}");
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: SnapshotEntry = die!(serde_json::from_str(line), "Error parsing JSON: {}");
            map.insert(entry.reading, entry.surfaces);
        }
        map
    };

    let readings = read_input_lines(input_file);

    let mut changed = 0;
    let mut same = 0;
    let mut new = 0;
    let total = readings.len();

    for reading in &readings {
        let nbest = if let Some(ref h) = user_history {
            convert_nbest_with_history(&dict, Some(&conn), h, reading, n)
        } else {
            convert_nbest(&dict, Some(&conn), reading, n)
        };

        let current: Vec<String> = nbest
            .iter()
            .map(|path| path.iter().map(|s| s.surface.as_str()).collect())
            .collect();

        match expected.get(reading) {
            Some(exp) => {
                if *exp != current {
                    changed += 1;
                    let exp_first = exp.first().map(|s| s.as_str()).unwrap_or("(empty)");
                    let cur_first = current.first().map(|s| s.as_str()).unwrap_or("(empty)");
                    if exp_first != cur_first {
                        println!(
                            "  CHANGED: {} → {} (was: {})",
                            reading, cur_first, exp_first
                        );
                    } else {
                        // First candidate same, but later ones differ
                        println!(
                            "  changed: {} → {} (same #1, later candidates differ)",
                            reading, cur_first
                        );
                    }
                } else {
                    same += 1;
                }
            }
            None => {
                new += 1;
                let cur_first = current.first().map(|s| s.as_str()).unwrap_or("(empty)");
                println!("  NEW:     {} → {}", reading, cur_first);
            }
        }
    }

    println!();
    println!("=== Summary ===");
    println!("  Total:    {total}");
    println!("  Same:     {same}");
    println!("  Changed:  {changed}");
    println!("  New:      {new}");

    if changed > 0 {
        process::exit(1);
    }
}

fn read_input_lines(path: &str) -> Vec<String> {
    let file = die!(fs::File::open(path), "Error opening input file: {}");
    let reader = BufReader::new(file);
    reader
        .lines()
        .map(|l| die!(l, "Error reading line: {}"))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn explain_cmd(
    dict_file: &str,
    conn_file: &str,
    reading: &str,
    surface_filter: Option<&str>,
    n: usize,
    history: Option<&str>,
) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );

    let user_history = history.map(|path| {
        die!(
            UserHistory::open(Path::new(path)),
            "Error opening history: {}"
        )
    });

    let result = explain::explain(
        &dict,
        Some(&conn),
        user_history.as_ref(),
        reading,
        n.max(20), // get more paths for filtering
    );

    // === Lattice ===
    println!("Reading: {}", result.reading);
    println!();

    // Group nodes by start position
    let char_count = reading.chars().count();
    println!("=== Lattice ({} nodes) ===", result.lattice_nodes.len());
    for pos in 0..char_count {
        let nodes_at_pos: Vec<_> = result
            .lattice_nodes
            .iter()
            .filter(|n| n.start == pos)
            .collect();
        if nodes_at_pos.is_empty() {
            continue;
        }
        println!("  Position {}:", pos);
        for node in &nodes_at_pos {
            let surface_display = if node.surface != node.reading {
                format!(" → {}", node.surface)
            } else {
                String::new()
            };
            println!(
                "    [{},{}] {}  cost={:<6} L={:<4} R={:<4}{}",
                node.start,
                node.end,
                node.reading,
                node.cost,
                node.left_id,
                node.right_id,
                surface_display,
            );
        }
    }

    // === Paths ===
    println!();
    let mut paths = result.paths;

    // Filter by surface if requested
    if let Some(filter) = surface_filter {
        paths.retain(|p| p.surface().contains(filter));
    }

    paths.truncate(n);

    if paths.is_empty() {
        println!("No paths found.");
        return;
    }

    println!("=== Paths ({}) ===", paths.len());
    for (i, path) in paths.iter().enumerate() {
        let surface = path.surface();
        println!();
        println!(
            "  #{:<2} {}  (final_cost={})",
            i + 1,
            surface,
            path.final_cost,
        );

        // Segment breakdown
        for (j, seg) in path.segments.iter().enumerate() {
            let seg_label = if seg.surface != seg.reading {
                format!("{}({})", seg.surface, seg.reading)
            } else {
                seg.surface.clone()
            };
            let conn_label = if j == 0 { "BOS→" } else { "conn=" };
            println!(
                "    seg[{}]: {:<12} word={:<6} penalty={:<5} script={:<6} {}{}",
                j,
                seg_label,
                seg.word_cost,
                seg.segment_penalty,
                seg.script_cost,
                conn_label,
                seg.connection_cost,
            );
        }

        // Cost summary
        println!(
            "    viterbi={:<8} rerank={:<+8} history={:<+8} → final={}",
            path.viterbi_cost, path.rerank_delta, -path.history_boost, path.final_cost,
        );
    }
}

fn convert_cmd(dict_file: &str, conn_file: &str, kana: &str, n: usize, history: Option<&str>) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );

    let user_history = history.map(|path| {
        die!(
            UserHistory::open(Path::new(path)),
            "Error opening history: {}"
        )
    });

    if n <= 1 {
        let result = if let Some(ref h) = user_history {
            convert_with_history(&dict, Some(&conn), h, kana)
        } else {
            convert(&dict, Some(&conn), kana)
        };
        let segs: Vec<String> = result
            .iter()
            .map(|s| format!("{}({})", s.surface, s.reading))
            .collect();
        println!("{}", segs.join(" | "));
    } else {
        let nbest = if let Some(ref h) = user_history {
            convert_nbest_with_history(&dict, Some(&conn), h, kana, n)
        } else {
            convert_nbest(&dict, Some(&conn), kana, n)
        };
        for (i, path) in nbest.iter().enumerate() {
            let segs: Vec<String> = path
                .iter()
                .map(|s| format!("{}({})", s.surface, s.reading))
                .collect();
            println!("#{:>2}: {}", i + 1, segs.join(" | "));
        }
    }
}
