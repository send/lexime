use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use lex_core::converter::{convert_nbest, convert_nbest_with_history};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;
use lex_core::user_history::UserHistory;

#[derive(Parser)]
#[command(name = "lextool", about = "Lexime conversion diagnostics")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Explain the conversion pipeline for a reading
    Explain {
        /// Path to the compiled dictionary file
        dict_file: String,
        /// Kana reading to explain
        reading: String,
        /// Path to the compiled connection matrix file (optional)
        #[arg(long)]
        conn: Option<String>,
        /// Filter to paths containing this surface (optional)
        #[arg(long)]
        surface: Option<String>,
        /// Path to user history file (optional)
        #[arg(long)]
        history: Option<String>,
        /// Number of N-best paths to show
        #[arg(short, long, default_value = "10")]
        n: usize,
        /// Output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// Omit lattice_nodes from JSON output
        #[arg(long)]
        no_lattice: bool,
    },

    /// Run readings from a file and record top-N results to JSONL
    Snapshot {
        /// Path to the compiled dictionary file
        dict_file: String,
        /// Path to the compiled connection matrix file
        conn_file: String,
        /// Path to the input file (one reading per line)
        input_file: String,
        /// Path to the output JSONL file
        output_file: String,
        /// Number of top results to record per reading
        #[arg(short, long, default_value = "5")]
        n: usize,
        /// Path to user history file (optional)
        #[arg(long)]
        history: Option<String>,
    },

    /// Run conversion accuracy tests from a structured TOML corpus
    Accuracy {
        /// Path to the compiled dictionary file
        dict_file: String,
        /// Path to the compiled connection matrix file
        conn_file: String,
        /// Path to the accuracy corpus TOML file
        corpus_file: String,
        /// Filter by tag (only run cases with this tag)
        #[arg(long)]
        tag: Option<String>,
        /// Filter by category (only run cases in this category)
        #[arg(long)]
        category: Option<String>,
        /// Show passing cases too (default: only failures and skips)
        #[arg(long)]
        verbose: bool,
        /// Output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// Path to user history file (optional)
        #[arg(long)]
        history: Option<String>,
    },

    /// Compare current output against a saved snapshot
    DiffSnapshot {
        /// Path to the compiled dictionary file
        dict_file: String,
        /// Path to the compiled connection matrix file
        conn_file: String,
        /// Path to the input file (one reading per line)
        input_file: String,
        /// Path to the baseline JSONL snapshot file
        baseline_file: String,
        /// Number of top results to compare per reading
        #[arg(short, long, default_value = "5")]
        n: usize,
        /// Path to user history file (optional)
        #[arg(long)]
        history: Option<String>,
    },
}

/// A single snapshot entry (one per reading).
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotEntry {
    reading: String,
    surfaces: Vec<String>,
}

// --- Accuracy types ---

#[derive(Debug, Deserialize)]
struct AccuracyCorpus {
    cases: Vec<AccuracyCase>,
    #[serde(default)]
    history: Vec<HistoryRecord>,
}

#[derive(Debug, Deserialize)]
struct HistoryRecord {
    segments: Vec<(String, String)>,
    #[serde(default = "default_repeat")]
    repeat: u32,
}

fn default_repeat() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct AccuracyCase {
    reading: String,
    expected: String,
    category: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    skip: bool,
    #[serde(default)]
    baseline: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    pr: Option<String>,
}

#[derive(Debug, Serialize)]
struct AccuracyResult {
    reading: String,
    expected: String,
    actual: String,
    status: AccuracyStatus,
    category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum AccuracyStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Serialize)]
struct AccuracySummary {
    total: usize,
    pass: usize,
    fail: usize,
    skip: usize,
    pass_rate: String,
}

#[derive(Debug, Serialize)]
struct AccuracyReport {
    results: Vec<AccuracyResult>,
    summary: AccuracySummary,
}

fn open_resources(
    dict_file: &str,
    conn_file: Option<&str>,
    history: &Option<String>,
) -> (
    TrieDictionary,
    Option<ConnectionMatrix>,
    Option<UserHistory>,
) {
    let dict = TrieDictionary::open(Path::new(dict_file)).unwrap_or_else(|e| {
        eprintln!("Failed to open dictionary at {}: {}", dict_file, e);
        process::exit(1);
    });

    let conn = conn_file.map(|cf| {
        ConnectionMatrix::open(Path::new(cf)).unwrap_or_else(|e| {
            eprintln!("Failed to open connection matrix at {}: {}", cf, e);
            process::exit(1);
        })
    });

    let hist = history.as_ref().map(|path| {
        UserHistory::open(Path::new(path)).unwrap_or_else(|e| {
            eprintln!("Failed to open user history at {}: {}", path, e);
            process::exit(1);
        })
    });

    (dict, conn, hist)
}

fn read_readings(input_file: &str) -> Vec<String> {
    let file = fs::File::open(input_file).unwrap_or_else(|e| {
        eprintln!("Failed to open input file {}: {}", input_file, e);
        process::exit(1);
    });
    BufReader::new(file)
        .lines()
        .map(|l| {
            l.unwrap_or_else(|e| {
                eprintln!("Failed to read line: {}", e);
                process::exit(1);
            })
        })
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn run_snapshot(
    dict: &TrieDictionary,
    conn: &ConnectionMatrix,
    hist: Option<&UserHistory>,
    reading: &str,
    n: usize,
) -> SnapshotEntry {
    let paths = match hist {
        Some(h) => convert_nbest_with_history(dict, Some(conn), h, reading, n),
        None => convert_nbest(dict, Some(conn), reading, n),
    };
    let surfaces: Vec<String> = paths
        .iter()
        .map(|segs| segs.iter().map(|s| s.surface.as_str()).collect())
        .collect();
    SnapshotEntry {
        reading: reading.to_string(),
        surfaces,
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Explain {
            dict_file,
            reading,
            conn,
            surface,
            history,
            n,
            json,
            no_lattice,
        } => {
            use lex_core::converter::explain;

            let (dict, conn, hist) = open_resources(&dict_file, conn.as_deref(), &history);
            // Over-fetch when filtering by surface
            let fetch_n = if surface.is_some() { n.max(20) } else { n };
            let mut result =
                explain::explain(&dict, conn.as_ref(), hist.as_ref(), &reading, fetch_n);

            if let Some(ref filter) = surface {
                result.paths.retain(|p| p.surface().contains(filter));
                result.paths.truncate(n);
            }

            if no_lattice {
                result.lattice_nodes.clear();
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).expect("JSON serialization failed")
                );
            } else {
                print!("{}", explain::format_text(&result));
            }
        }

        Command::Accuracy {
            dict_file,
            conn_file,
            corpus_file,
            tag,
            category,
            verbose,
            json,
            history,
        } => {
            let (dict, conn, file_hist) = open_resources(&dict_file, Some(&conn_file), &history);
            let conn = conn.expect("connection matrix is required for accuracy");

            // Load and parse corpus
            let corpus_content = fs::read_to_string(&corpus_file).unwrap_or_else(|e| {
                eprintln!("Failed to read corpus file {}: {}", corpus_file, e);
                process::exit(1);
            });
            let corpus: AccuracyCorpus = toml::from_str(&corpus_content).unwrap_or_else(|e| {
                eprintln!("Failed to parse corpus TOML: {}", e);
                process::exit(1);
            });

            // Build history: corpus-embedded or CLI --history (not both)
            let hist = if !corpus.history.is_empty() {
                if file_hist.is_some() {
                    eprintln!(
                        "Error: corpus contains [[history]] entries and --history flag was also given. Use one or the other."
                    );
                    process::exit(1);
                }
                let mut h = UserHistory::new();
                let now = lex_core::user_history::now_epoch();
                for rec in &corpus.history {
                    for _ in 0..rec.repeat {
                        h.record_at(&rec.segments, now);
                    }
                }
                Some(h)
            } else {
                file_hist
            };

            // Filter cases
            let cases: Vec<&AccuracyCase> = corpus
                .cases
                .iter()
                .filter(|c| {
                    if let Some(ref t) = tag {
                        if !c.tags.contains(t) {
                            return false;
                        }
                    }
                    if let Some(ref cat) = category {
                        if c.category != *cat {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            if cases.is_empty() {
                eprintln!("No cases match the given filters");
                process::exit(1);
            }

            // Run each case
            let mut results: Vec<AccuracyResult> = Vec::new();
            for case in &cases {
                if case.skip {
                    results.push(AccuracyResult {
                        reading: case.reading.clone(),
                        expected: case.expected.clone(),
                        actual: String::new(),
                        status: AccuracyStatus::Skip,
                        category: case.category.clone(),
                        baseline: case.baseline.clone(),
                        baseline_actual: None,
                        note: case.note.clone(),
                        issue: case.issue.clone(),
                        pr: case.pr.clone(),
                    });
                    continue;
                }

                // If baseline is specified, first verify no-history conversion
                let (baseline_actual, baseline_changed) =
                    if let Some(ref expected_baseline) = case.baseline {
                        let paths_no_hist = convert_nbest(&dict, Some(&conn), &case.reading, 1);
                        let ba: String = paths_no_hist
                            .first()
                            .map(|segs| segs.iter().map(|s| s.surface.as_str()).collect())
                            .unwrap_or_default();
                        let changed = ba != *expected_baseline;
                        (Some(ba), changed)
                    } else {
                        (None, false)
                    };

                if baseline_changed {
                    results.push(AccuracyResult {
                        reading: case.reading.clone(),
                        expected: case.expected.clone(),
                        actual: String::new(),
                        status: AccuracyStatus::Fail,
                        category: case.category.clone(),
                        baseline: case.baseline.clone(),
                        baseline_actual: baseline_actual.clone(),
                        note: case.note.clone(),
                        issue: case.issue.clone(),
                        pr: case.pr.clone(),
                    });
                    continue;
                }

                let paths = match hist.as_ref() {
                    Some(h) => convert_nbest_with_history(&dict, Some(&conn), h, &case.reading, 1),
                    None => convert_nbest(&dict, Some(&conn), &case.reading, 1),
                };
                let actual: String = paths
                    .first()
                    .map(|segs| segs.iter().map(|s| s.surface.as_str()).collect())
                    .unwrap_or_default();

                let status = if actual == case.expected {
                    AccuracyStatus::Pass
                } else {
                    AccuracyStatus::Fail
                };

                results.push(AccuracyResult {
                    reading: case.reading.clone(),
                    expected: case.expected.clone(),
                    actual,
                    status,
                    category: case.category.clone(),
                    baseline: case.baseline.clone(),
                    baseline_actual,
                    note: case.note.clone(),
                    issue: case.issue.clone(),
                    pr: case.pr.clone(),
                });
            }

            // Compute summary
            let total = results.len();
            let pass = results
                .iter()
                .filter(|r| matches!(r.status, AccuracyStatus::Pass))
                .count();
            let fail = results
                .iter()
                .filter(|r| matches!(r.status, AccuracyStatus::Fail))
                .count();
            let skip = results
                .iter()
                .filter(|r| matches!(r.status, AccuracyStatus::Skip))
                .count();
            let tested = total - skip;
            let rate = if tested > 0 {
                pass as f64 / tested as f64 * 100.0
            } else {
                0.0
            };
            let summary = AccuracySummary {
                total,
                pass,
                fail,
                skip,
                pass_rate: format!("{:.1}%", rate),
            };

            if json {
                let report = AccuracyReport { results, summary };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("JSON serialization failed")
                );
            } else {
                // Group by category
                let mut grouped: BTreeMap<&str, Vec<&AccuracyResult>> = BTreeMap::new();
                for r in &results {
                    grouped.entry(&r.category).or_default().push(r);
                }

                for (cat, group) in &grouped {
                    let cat_total = group.len();
                    println!("\n=== {} ({} cases) ===", cat, cat_total);
                    for r in group {
                        match r.status {
                            AccuracyStatus::Pass => {
                                if verbose {
                                    if let Some(ref bl) = r.baseline {
                                        println!(
                                            "  \u{2713} {}: {} \u{2192} {}",
                                            r.reading, bl, r.expected
                                        );
                                    } else {
                                        println!(
                                            "  \u{2713} {} \u{2192} {}",
                                            r.reading, r.expected
                                        );
                                    }
                                }
                            }
                            AccuracyStatus::Fail => {
                                // Baseline changed?
                                if let (Some(ref bl), Some(ref ba)) =
                                    (&r.baseline, &r.baseline_actual)
                                {
                                    if ba != bl {
                                        println!(
                                            "  \u{2717} {}: baseline changed (expected: {}, got: {})",
                                            r.reading, bl, ba
                                        );
                                        continue;
                                    }
                                }
                                println!(
                                    "  \u{2717} {} \u{2192} {} (got: {})",
                                    r.reading, r.expected, r.actual
                                );
                            }
                            AccuracyStatus::Skip => {
                                let reason = r
                                    .note
                                    .as_deref()
                                    .or(r.issue.as_deref())
                                    .unwrap_or("known failure");
                                println!("  - {} [skip: {}]", r.reading, reason);
                            }
                        }
                    }
                }

                println!();
                println!("=== Summary ===");
                println!("  Total:     {}", summary.total);
                println!("  Pass:      {:>3}", summary.pass);
                println!("  Fail:      {:>3}", summary.fail);
                println!("  Skip:      {:>3}", summary.skip);
                println!(
                    "  Pass rate: {} ({}/{})",
                    summary.pass_rate, summary.pass, tested
                );
            }

            if fail > 0 {
                process::exit(1);
            }
        }

        Command::Snapshot {
            dict_file,
            conn_file,
            input_file,
            output_file,
            n,
            history,
        } => {
            let (dict, conn, hist) = open_resources(&dict_file, Some(&conn_file), &history);
            let conn = conn.expect("connection matrix is required for snapshot");
            let readings = read_readings(&input_file);

            let file = fs::File::create(&output_file).unwrap_or_else(|e| {
                eprintln!("Failed to create output file {}: {}", output_file, e);
                process::exit(1);
            });
            let mut writer = BufWriter::new(file);

            for reading in &readings {
                let entry = run_snapshot(&dict, &conn, hist.as_ref(), reading, n);
                let line = serde_json::to_string(&entry).expect("JSON serialization failed");
                writeln!(writer, "{}", line).unwrap_or_else(|e| {
                    eprintln!("Failed to write: {}", e);
                    process::exit(1);
                });
            }

            eprintln!(
                "Snapshot written: {} readings -> {}",
                readings.len(),
                output_file
            );
        }

        Command::DiffSnapshot {
            dict_file,
            conn_file,
            input_file,
            baseline_file,
            n,
            history,
        } => {
            let (dict, conn, hist) = open_resources(&dict_file, Some(&conn_file), &history);
            let conn = conn.expect("connection matrix is required for diff-snapshot");
            let readings = read_readings(&input_file);

            // Load baseline
            let baseline_content = fs::read_to_string(&baseline_file).unwrap_or_else(|e| {
                eprintln!("Failed to read baseline file {}: {}", baseline_file, e);
                process::exit(1);
            });
            let mut baseline: HashMap<String, SnapshotEntry> = HashMap::new();
            for line in baseline_content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let entry: SnapshotEntry = serde_json::from_str(line).unwrap_or_else(|e| {
                    eprintln!("Failed to parse baseline JSONL: {}", e);
                    process::exit(1);
                });
                baseline.insert(entry.reading.clone(), entry);
            }

            let mut changed = 0usize;
            let mut same = 0usize;
            let mut new_count = 0usize;
            let total = readings.len();

            for reading in &readings {
                let current = run_snapshot(&dict, &conn, hist.as_ref(), reading, n);

                match baseline.get(reading) {
                    Some(base) => {
                        if base.surfaces != current.surfaces {
                            changed += 1;
                            let base_first = base
                                .surfaces
                                .first()
                                .map(|s| s.as_str())
                                .unwrap_or("(empty)");
                            let curr_first = current
                                .surfaces
                                .first()
                                .map(|s| s.as_str())
                                .unwrap_or("(empty)");
                            if base_first != curr_first {
                                println!(
                                    "  CHANGED: {} -> {} (was: {})",
                                    reading, curr_first, base_first
                                );
                            } else {
                                println!(
                                    "  changed: {} -> {} (same #1, later candidates differ)",
                                    reading, curr_first
                                );
                            }
                        } else {
                            same += 1;
                        }
                    }
                    None => {
                        new_count += 1;
                        let curr_first = current
                            .surfaces
                            .first()
                            .map(|s| s.as_str())
                            .unwrap_or("(empty)");
                        println!("  NEW:     {} -> {}", reading, curr_first);
                    }
                }
            }

            // Detect removed readings (in baseline but not in input)
            let input_set: HashSet<&str> = readings.iter().map(|s| s.as_str()).collect();
            let mut removed = 0usize;
            for key in baseline.keys() {
                if !input_set.contains(key.as_str()) {
                    removed += 1;
                    println!("  REMOVED: {}", key);
                }
            }

            println!();
            println!("=== Summary ===");
            println!("  Total:    {total}");
            println!("  Same:     {same}");
            println!("  Changed:  {changed}");
            println!("  New:      {new_count}");
            println!("  Removed:  {removed}");

            if changed > 0 || removed > 0 {
                process::exit(1);
            }
        }
    }
}
