use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use lex_engine::converter::explain;
use lex_engine::dict::connection::ConnectionMatrix;
use lex_engine::dict::TrieDictionary;
use lex_engine::user_history::UserHistory;

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
        /// Path to the compiled connection matrix file
        conn_file: String,
        /// Kana reading to explain
        reading: String,
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

fn open_resources(
    dict_file: &str,
    conn_file: &str,
    history: &Option<String>,
) -> (TrieDictionary, ConnectionMatrix, Option<UserHistory>) {
    let dict = TrieDictionary::open(Path::new(dict_file)).unwrap_or_else(|e| {
        eprintln!("Failed to open dictionary at {}: {}", dict_file, e);
        process::exit(1);
    });

    let conn = ConnectionMatrix::open(Path::new(conn_file)).unwrap_or_else(|e| {
        eprintln!("Failed to open connection matrix at {}: {}", conn_file, e);
        process::exit(1);
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
    let result = explain::explain(dict, Some(conn), hist, reading, n);
    let surfaces: Vec<String> = result.paths.iter().map(|p| p.surface()).collect();
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
            conn_file,
            reading,
            surface,
            history,
            n,
            json,
        } => {
            let (dict, conn, hist) = open_resources(&dict_file, &conn_file, &history);
            // Over-fetch when filtering by surface
            let fetch_n = if surface.is_some() { n.max(20) } else { n };
            let mut result = explain::explain(&dict, Some(&conn), hist.as_ref(), &reading, fetch_n);

            if let Some(ref filter) = surface {
                result.paths.retain(|p| p.surface().contains(filter));
                result.paths.truncate(n);
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

        Command::Snapshot {
            dict_file,
            conn_file,
            input_file,
            output_file,
            n,
            history,
        } => {
            let (dict, conn, hist) = open_resources(&dict_file, &conn_file, &history);
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
            let (dict, conn, hist) = open_resources(&dict_file, &conn_file, &history);
            let readings = read_readings(&input_file);

            // Load baseline
            let baseline_content = fs::read_to_string(&baseline_file).unwrap_or_else(|e| {
                eprintln!("Failed to read baseline file {}: {}", baseline_file, e);
                process::exit(1);
            });
            let mut baseline: std::collections::HashMap<String, SnapshotEntry> =
                std::collections::HashMap::new();
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

            println!();
            println!("=== Summary ===");
            println!("  Total:    {total}");
            println!("  Same:     {same}");
            println!("  Changed:  {changed}");
            println!("  New:      {new_count}");

            if changed > 0 {
                process::exit(1);
            }
        }
    }
}
