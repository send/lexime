use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use lex_cli::commands::rank_ops::{self, joined_surface, WindowCheck};
use lex_core::converter::tune;
use lex_core::converter::{convert_nbest, convert_nbest_with_history};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use lex_core::user_history::UserHistory;

#[derive(Parser)]
#[command(name = "lextool", about = "Lexime conversion diagnostics")]
struct Cli {
    /// Path to a settings.toml to run under instead of the embedded
    /// settings, as the app loads it (any subcommand)
    #[arg(long, global = true)]
    settings: Option<String>,
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
        /// Record the production candidate list (N-best + kana + predictions
        /// + lookup) instead of the N-best paths
        #[arg(long)]
        candidates: bool,
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

    /// Grid-search FeatureWeights to optimise conversion accuracy
    Tune {
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
        /// Output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// Number of top weight combinations to show
        #[arg(long, default_value = "10")]
        top_n: usize,
    },

    /// Audit user history: compare raw engine top-1 against the user's
    /// dominant committed surface for each learned reading
    HistoryAudit {
        /// Path to the compiled dictionary file
        dict_file: String,
        /// Path to the compiled connection matrix file
        conn_file: String,
        /// Path to the user history checkpoint (.lxud; adjacent WAL is replayed)
        history_file: String,
        /// Minimum commit frequency for a reading to be audited
        #[arg(long, default_value = "2")]
        min_freq: u32,
        /// N-best depth used to locate the rank of the dominant choice
        #[arg(short, long, default_value = "10")]
        n: usize,
        /// Output as JSON instead of text
        #[arg(long)]
        json: bool,
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
        /// Compare production candidate lists (baseline must be a
        /// `snapshot --candidates` file)
        #[arg(long)]
        candidates: bool,
    },

    /// Replay rank>0 selections from the commit log against the current
    /// engine. Prints counts only; the log holds personal input.
    ReplayCommitLog {
        /// Path to the compiled dictionary file
        dict_file: String,
        /// Path to the compiled connection matrix file
        conn_file: String,
        /// Path to commit-log.jsonl
        log_file: String,
        /// Path to user history file (optional; default is no history)
        #[arg(long)]
        history: Option<String>,
        /// The app's data directory: replay under the configuration the app
        /// runs with — its user_dict.lxuw layered over the system dictionary
        /// and its settings.toml — including the app's fallbacks when a file
        /// is missing, unreadable or invalid (warned here, as the app reports
        /// them). Without it: system dictionary and embedded settings
        /// (not with --settings)
        #[arg(long)]
        app_dir: Option<String>,
        /// Compare against a baseline written by --emit-baseline. Lines join
        /// on (line index, timestamp in seconds): a log cleared and rewritten
        /// is told apart unless its line at the same index lands in the same
        /// second as the old one
        #[arg(long)]
        baseline: Option<String>,
        /// Write a baseline (line index, timestamp, rank — no reading or
        /// surface). The timestamps still record when you corrected the IME:
        /// keep the file local, do not attach it to a PR or issue
        #[arg(long)]
        emit_baseline: Option<String>,
        /// Print each selection's reading and surface to stderr (local only;
        /// never paste this output into a PR or issue)
        #[arg(long)]
        verbose: bool,
        /// Output as JSON instead of text
        #[arg(long)]
        json: bool,
    },
}

/// A single snapshot entry (one per reading).
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotEntry {
    reading: String,
    surfaces: Vec<String>,
    /// What `surfaces` holds. Snapshots are regenerable dev artifacts, so a
    /// file written before this field existed is regenerated, not defaulted.
    kind: SnapshotKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SnapshotKind {
    Nbest,
    Candidates,
}

impl SnapshotKind {
    fn from_flag(candidates: bool) -> Self {
        if candidates {
            Self::Candidates
        } else {
            Self::Nbest
        }
    }
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
    /// Rank-2+ expectations on the production candidate list.
    #[serde(default)]
    window: Option<WindowCheck>,
    /// The production list's #1 when it legitimately differs from
    /// `expected` (history promoting learned kana to index 0).
    #[serde(default)]
    window_top1: Option<String>,
    /// Known width disagreement: report it without failing the case.
    #[serde(default)]
    width_issue: Option<WidthIssue>,
}

/// A known width disagreement, named exactly: the issue tracking it and the
/// top-1 each disagreeing width shows. Only that disagreement is exempt —
/// any other width, a different top-1, or the no-history baseline still
/// fails the case.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WidthIssue {
    issue: String,
    /// What the synchronous 1-best shows instead of `expected`.
    #[serde(default)]
    one_best: Option<String>,
    /// What the production list's #1 is instead of `expected`.
    #[serde(default)]
    list_top: Option<String>,
}

impl WidthIssue {
    fn covers(&self, m: &WidthMismatch) -> bool {
        match m {
            WidthMismatch::OneBest(got) => self.one_best.as_deref() == Some(got),
            WidthMismatch::ListTop { got, .. } => self.list_top.as_deref() == Some(got),
        }
    }

    /// Declared disagreements that did not occur.
    fn stale(&self, seen: &[WidthMismatch]) -> Vec<String> {
        let seen_one_best = seen
            .iter()
            .any(|m| matches!(m, WidthMismatch::OneBest(_)) && self.covers(m));
        let seen_list_top = seen
            .iter()
            .any(|m| matches!(m, WidthMismatch::ListTop { .. }) && self.covers(m));
        let mut out = Vec::new();
        if let Some(v) = self.one_best.as_ref().filter(|_| !seen_one_best) {
            out.push(format!("1-best no longer shows {v}"));
        }
        if let Some(v) = self.list_top.as_ref().filter(|_| !seen_list_top) {
            out.push(format!("candidate #1 is no longer {v}"));
        }
        out
    }
}

/// One width whose top-1 differs from what the case expects.
enum WidthMismatch {
    OneBest(String),
    ListTop { got: String, want: String },
}

impl std::fmt::Display for WidthMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OneBest(got) => write!(f, "1-best shows {got}"),
            Self::ListTop { got, want } => write!(f, "candidate #1 is {got} (want {want})"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
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
    /// Which gate failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<Failure>,
    /// Width disagreements and window violations, human-readable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    details: Vec<String>,
}

/// The gate a failing case tripped.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Failure {
    /// The no-history `baseline` moved.
    Baseline,
    /// The N-best head at n=1 is not `expected`.
    Top1,
    /// Another display width disagrees on top-1.
    Width,
    /// A `[cases.window]` expectation is violated.
    Window,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Top1 => "top1",
            Self::Width => "width",
            Self::Window => "window",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
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

// --- History audit types ---

/// A reading where the raw engine top-1 disagrees with the user's dominant choice.
#[derive(Debug, Serialize)]
struct AuditMiss {
    reading: String,
    /// The surface the user committed most often for this reading.
    dominant: String,
    /// Commit frequency of the dominant surface.
    frequency: u32,
    /// Raw (no-history) top-1 conversion.
    raw_top1: String,
    /// 1-based rank of the dominant surface in the raw N-best; None = absent.
    rank: Option<usize>,
    /// Whether the history-boosted top-1 matches the dominant surface.
    history_fixed: bool,
}

/// A reading the user regularly commits with more than one surface.
#[derive(Debug, Serialize)]
struct AuditFlipFlop {
    reading: String,
    surfaces: Vec<(String, u32)>,
}

#[derive(Debug, Serialize)]
struct AuditReport {
    audited: usize,
    agree: usize,
    agree_rate: String,
    misses: Vec<AuditMiss>,
    flip_flops: Vec<AuditFlipFlop>,
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

    let hist = history.as_deref().map(open_history);

    (dict, conn, hist)
}

/// Open a user history the user named. `open_with_wal` returns an empty
/// history for a missing checkpoint and WAL, so a mistyped path must fail
/// here instead of measuring an unlearned engine as if it were the learned
/// one. The WAL is replayed: uncheckpointed commits live only there.
fn open_history(path: &str) -> UserHistory {
    let checkpoint = Path::new(path);
    let wal = checkpoint.with_extension("lxud.wal");
    if !checkpoint.exists() && !wal.exists() {
        eprintln!(
            "User history not found: neither {} nor {} exists",
            checkpoint.display(),
            wal.display()
        );
        process::exit(1);
    }
    let (h, _wal) = lex_core::user_history::wal::open_with_wal(checkpoint).unwrap_or_else(|e| {
        eprintln!("Failed to open user history at {}: {}", path, e);
        process::exit(1);
    });
    h
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
    kind: SnapshotKind,
) -> SnapshotEntry {
    let surfaces: Vec<String> = match kind {
        SnapshotKind::Nbest => {
            let paths = match hist {
                Some(h) => convert_nbest_with_history(dict, Some(conn), h, reading, n),
                None => convert_nbest(dict, Some(conn), reading, n),
            };
            paths.iter().map(|segs| joined_surface(segs)).collect()
        }
        SnapshotKind::Candidates => rank_ops::production_candidates(dict, conn, hist, reading)
            .surfaces
            .into_iter()
            .take(n)
            .collect(),
    };
    SnapshotEntry {
        reading: reading.to_string(),
        surfaces,
        kind,
    }
}

/// The app's load policy for the files in its data directory
/// (`AppContext` / `EngineContainer`), mirrored for `replay-commit-log
/// --app-dir`: a missing file is not used; an unreadable or invalid one is
/// not used either, and the app keeps running and reports it — here, a
/// warning. Nothing is written: the app quarantines a corrupt user
/// dictionary, a measurement tool never touches user data.
mod app_config {
    use std::fs;
    use std::io::ErrorKind;
    use std::path::Path;

    use lex_core::user_dict::UserDictionary;

    fn warn(what: &Path, e: impl std::fmt::Display, fallback: &str) {
        eprintln!(
            "replay-commit-log: warning: {}: {e}; {fallback}, as the app does",
            what.display()
        );
    }

    /// `settings.toml`, if present and valid; otherwise the embedded settings.
    pub fn settings(dir: &Path) {
        let path = dir.join("settings.toml");
        if !path.exists() {
            return;
        }
        let loaded = fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|toml| lex_core::settings::init_custom(toml).map_err(|e| e.to_string()));
        if let Err(e) = loaded {
            warn(&path, e, "replaying with the embedded settings");
        }
    }

    /// `user_dict.lxuw` (empty when missing or corrupt), or `None` when it
    /// cannot be read (the app then runs on the system dictionary alone).
    pub fn user_dict(dir: &Path) -> Option<UserDictionary> {
        let path = dir.join("user_dict.lxuw");
        match UserDictionary::open(&path) {
            Ok(ud) => Some(ud),
            Err(e) if e.kind() == ErrorKind::InvalidData => {
                warn(&path, e, "replaying with an empty user dictionary");
                Some(UserDictionary::new())
            }
            Err(e) => {
                warn(&path, e, "replaying with the system dictionary only");
                None
            }
        }
    }
}

fn main() {
    let cli = Cli::parse();
    // Before any subcommand runs: settings() fixes on its first read, and a
    // custom TOML set after that would be ignored without an error.
    let app_dir = match &cli.command {
        Command::ReplayCommitLog { app_dir, .. } => app_dir.as_deref(),
        _ => None,
    };
    if cli.settings.is_some() && app_dir.is_some() {
        eprintln!("--settings and --app-dir both name a settings file; pass one");
        process::exit(2);
    }
    if let Some(path) = &cli.settings {
        let loaded = fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|toml| lex_core::settings::init_custom(toml).map_err(|e| e.to_string()));
        if let Err(e) = loaded {
            eprintln!("--settings {path}: {e}");
            process::exit(1);
        }
    } else if let Some(dir) = app_dir {
        app_config::settings(Path::new(dir));
    }

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

            // Validate window checks before running anything: a malformed
            // expectation must not read as a conversion failure.
            for case in &cases {
                // window_top1 exists for one thing: learned kana promoted to
                // the list's #1. Anything else would be an unlinked width
                // exemption, so it must be the reading itself, learned in
                // this corpus's history.
                if let Some(ref top) = case.window_top1 {
                    let learned = corpus.history.iter().any(|rec| {
                        rec.segments
                            .iter()
                            .any(|(r, s)| *r == case.reading && *s == case.reading)
                    });
                    if *top != case.reading || !learned {
                        eprintln!(
                            "window_top1 for {} must be the reading itself, learned as kana in \
                             this corpus's [[history]]; use width_issue for any other width \
                             disagreement",
                            case.reading
                        );
                        process::exit(1);
                    }
                }
                if let Some(ref w) = case.width_issue {
                    if !is_issue_ref(&w.issue) {
                        eprintln!(
                            "width_issue for {} must be an issue link like \"#123\", got {:?}",
                            case.reading, w.issue
                        );
                        process::exit(1);
                    }
                    if w.one_best.is_none() && w.list_top.is_none() {
                        eprintln!(
                            "width_issue for {} must name the disagreement it covers \
                             (one_best and/or list_top)",
                            case.reading
                        );
                        process::exit(1);
                    }
                }
                if let Some(ref w) = case.window {
                    // The no-history window is required where the corpus seeds
                    // its own history, not when --history is merely supplied.
                    if let Err(e) = w.validate(!corpus.history.is_empty()) {
                        eprintln!("Invalid [cases.window] for {}: {}", case.reading, e);
                        process::exit(1);
                    }
                }
            }

            let results: Vec<AccuracyResult> = cases
                .iter()
                .map(|case| eval_case(&dict, &conn, hist.as_ref(), case))
                .collect();

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
                                // Reported-only width disagreements stay
                                // visible without failing the run.
                                for d in &r.details {
                                    println!("  ! {}: {}", r.reading, d);
                                }
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
                                if r.failure == Some(Failure::Top1) {
                                    println!(
                                        "  \u{2717} {} \u{2192} {} (got: {})",
                                        r.reading, r.expected, r.actual
                                    );
                                } else {
                                    println!(
                                        "  \u{2717} {} \u{2192} {} [{}]",
                                        r.reading,
                                        r.expected,
                                        r.failure.map_or("?", Failure::label)
                                    );
                                }
                                for d in &r.details {
                                    println!("      {}", d);
                                }
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
            candidates,
        } => {
            let kind = SnapshotKind::from_flag(candidates);
            let (dict, conn, hist) = open_resources(&dict_file, Some(&conn_file), &history);
            let conn = conn.expect("connection matrix is required for snapshot");
            let readings = read_readings(&input_file);

            let entries: Vec<SnapshotEntry> = readings
                .iter()
                .map(|reading| run_snapshot(&dict, &conn, hist.as_ref(), reading, n, kind))
                .collect();
            write_jsonl(&output_file, &entries).unwrap_or_else(|e| {
                eprintln!("Failed to write snapshot: {}", e);
                process::exit(1);
            });

            eprintln!(
                "Snapshot written: {} readings -> {}",
                readings.len(),
                output_file
            );
        }

        Command::Tune {
            dict_file,
            conn_file,
            corpus_file,
            tag,
            category,
            json,
            top_n,
        } => {
            let (dict, conn, _) = open_resources(&dict_file, Some(&conn_file), &None);
            let conn = conn.expect("connection matrix is required for tune");

            // Load and parse corpus (same as Accuracy)
            let corpus_content = fs::read_to_string(&corpus_file).unwrap_or_else(|e| {
                eprintln!("Failed to read corpus file {}: {}", corpus_file, e);
                process::exit(1);
            });
            let corpus: AccuracyCorpus = toml::from_str(&corpus_content).unwrap_or_else(|e| {
                eprintln!("Failed to parse corpus TOML: {}", e);
                process::exit(1);
            });

            // Filter and collect non-skip cases
            let cases: Vec<(String, String)> = corpus
                .cases
                .iter()
                .filter(|c| {
                    if c.skip {
                        return false;
                    }
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
                .map(|c| (c.reading.clone(), c.expected.clone()))
                .collect();

            if cases.is_empty() {
                eprintln!("No cases match the given filters");
                process::exit(1);
            }

            let grid = tune::WeightGrid::default();
            let combos = grid.total_combinations();

            eprint!("Pre-computing candidates for {} cases... ", cases.len());
            let tune_cases = tune::precompute_cases(&dict, &conn, &cases);
            eprintln!("done");

            eprint!(
                "Grid search: {} combinations x {} cases... ",
                combos,
                cases.len()
            );
            let result = tune::grid_search(&tune_cases, &grid, top_n);
            eprintln!("done");

            if json {
                print_tune_json(&result);
            } else {
                print_tune_text(&result);
            }
        }

        Command::HistoryAudit {
            dict_file,
            conn_file,
            history_file,
            min_freq,
            n,
            json,
        } => {
            let (dict, conn, _) = open_resources(&dict_file, Some(&conn_file), &None);
            let conn = conn.expect("connection matrix is required for history-audit");

            let hist = open_history(&history_file);

            // Group unigrams by reading
            let mut by_reading: HashMap<&str, Vec<(&str, u32, u64)>> = HashMap::new();
            for (reading, surface, entry) in hist.unigrams() {
                by_reading.entry(reading).or_default().push((
                    surface,
                    entry.frequency,
                    entry.last_used,
                ));
            }

            let mut misses: Vec<AuditMiss> = Vec::new();
            let mut flip_flops: Vec<AuditFlipFlop> = Vec::new();
            let mut audited = 0usize;
            let mut agree = 0usize;

            for (reading, mut surfaces) in by_reading {
                // Tie-break by surface text: frequency and last_used can both
                // collide (records in one batch share the same second), and
                // HashMap order would make the dominant pick non-deterministic.
                surfaces.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(b.0)));
                let (dominant, frequency, _) = surfaces[0];
                if frequency < min_freq {
                    continue;
                }
                audited += 1;

                let regulars: Vec<(String, u32)> = surfaces
                    .iter()
                    .filter(|s| s.1 >= min_freq)
                    .map(|(s, f, _)| (s.to_string(), *f))
                    .collect();
                if regulars.len() >= 2 {
                    flip_flops.push(AuditFlipFlop {
                        reading: reading.to_string(),
                        surfaces: regulars,
                    });
                }

                let paths = convert_nbest(&dict, Some(&conn), reading, n);
                let joined: Vec<String> = paths.iter().map(|segs| joined_surface(segs)).collect();
                let raw_top1 = joined.first().cloned().unwrap_or_default();
                if raw_top1 == dominant {
                    agree += 1;
                    continue;
                }

                // Post-Viterbi rewriters can append candidates beyond the
                // requested depth, so paths may exceed n; scan only the first
                // n to keep "rank" and "not in top-n" semantics uniform.
                let rank = joined
                    .iter()
                    .take(n)
                    .position(|s| s == dominant)
                    .map(|i| i + 1);
                let hist_top1: String =
                    convert_nbest_with_history(&dict, Some(&conn), &hist, reading, 1)
                        .first()
                        .map(|segs| joined_surface(segs))
                        .unwrap_or_default();

                misses.push(AuditMiss {
                    reading: reading.to_string(),
                    dominant: dominant.to_string(),
                    frequency,
                    raw_top1,
                    rank,
                    history_fixed: hist_top1 == dominant,
                });
            }

            misses.sort_by(|a, b| {
                b.frequency
                    .cmp(&a.frequency)
                    .then_with(|| a.reading.cmp(&b.reading))
            });
            flip_flops.sort_by(|a, b| {
                let fa: u32 = a.surfaces.iter().map(|(_, f)| f).sum();
                let fb: u32 = b.surfaces.iter().map(|(_, f)| f).sum();
                fb.cmp(&fa).then_with(|| a.reading.cmp(&b.reading))
            });

            let rate = if audited > 0 {
                agree as f64 / audited as f64 * 100.0
            } else {
                0.0
            };
            let report = AuditReport {
                audited,
                agree,
                agree_rate: format!("{:.1}%", rate),
                misses,
                flip_flops,
            };

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("JSON serialization failed")
                );
            } else {
                print_audit_text(&report, min_freq, n);
            }
        }

        Command::DiffSnapshot {
            dict_file,
            conn_file,
            input_file,
            baseline_file,
            n,
            history,
            candidates,
        } => {
            let kind = SnapshotKind::from_flag(candidates);
            let (dict, conn, hist) = open_resources(&dict_file, Some(&conn_file), &history);
            let conn = conn.expect("connection matrix is required for diff-snapshot");
            let readings = read_readings(&input_file);

            // Load baseline
            let entries: Vec<SnapshotEntry> = read_jsonl(&baseline_file).unwrap_or_else(|e| {
                eprintln!("Failed to load baseline: {}", e);
                process::exit(1);
            });
            let mut baseline: HashMap<String, SnapshotEntry> = HashMap::new();
            for entry in entries {
                if entry.kind != kind {
                    eprintln!(
                        "Baseline holds {:?} snapshots but {:?} was requested (toggle --candidates)",
                        entry.kind, kind
                    );
                    process::exit(1);
                }
                baseline.insert(entry.reading.clone(), entry);
            }

            let mut changed = 0usize;
            let mut same = 0usize;
            let mut new_count = 0usize;
            let total = readings.len();

            for reading in &readings {
                let current = run_snapshot(&dict, &conn, hist.as_ref(), reading, n, kind);

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

        Command::ReplayCommitLog {
            dict_file,
            conn_file,
            log_file,
            history,
            app_dir,
            baseline,
            emit_baseline,
            verbose,
            json,
        } => {
            let die = |e: String| -> ! {
                eprintln!("replay-commit-log: {}", e);
                process::exit(1);
            };
            let (trie, conn, hist) = open_resources(&dict_file, Some(&conn_file), &history);
            let conn = conn.expect("connection matrix is required for replay-commit-log");
            // Same layering as LexDictionary::open_with_user_dict.
            let dict: Box<dyn Dictionary> = match app_dir
                .as_deref()
                .and_then(|d| app_config::user_dict(Path::new(d)))
            {
                Some(ud) => Box::new(CompositeDictionary::new(vec![Arc::new(trie), Arc::new(ud)])),
                None => Box::new(trie),
            };
            let (report, lines) =
                rank_ops::replay(&*dict, &conn, hist.as_ref(), Path::new(&log_file), verbose)
                    .unwrap_or_else(|e| die(e));
            let diff = baseline.map(|path| {
                let before: Vec<rank_ops::BaselineLine> =
                    read_jsonl(&path).unwrap_or_else(|e| die(e));
                rank_ops::diff_baseline(&before, &lines)
            });
            if let Some(path) = emit_baseline {
                write_jsonl(&path, &lines).unwrap_or_else(|e| die(e));
            }
            if json {
                #[derive(Serialize)]
                struct Out<'a> {
                    #[serde(flatten)]
                    report: &'a rank_ops::ReplayReport,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    baseline: Option<&'a rank_ops::BaselineDiff>,
                }
                let out = Out {
                    report: &report,
                    baseline: diff.as_ref(),
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out).expect("JSON serialization failed")
                );
            } else {
                print_replay_text(&report, diff.as_ref());
            }
        }
    }
}

fn print_replay_text(r: &rank_ops::ReplayReport, diff: Option<&rank_ops::BaselineDiff>) {
    let row = |label: &str, n: usize| {
        println!("  {:<20} {:>5} ({:.1}%)", label, n, pct(n, r.selections));
    };
    println!("=== Replay (rank>0 selections: {}) ===", r.selections);
    row(&format!("On page 1 (< {})", rank_ops::PAGE_SIZE), r.in_page);
    row("In list", r.in_list);
    row("Absent", r.absent);
    println!();
    println!("=== Position in the candidate list ===");
    for (rank, &n) in r.rank_hist.iter().enumerate().take(rank_ops::PAGE_SIZE) {
        row(&format!("#{}", rank + 1), n);
    }
    row(
        &format!("#{} or later", rank_ops::PAGE_SIZE + 1),
        r.rank_hist.iter().skip(rank_ops::PAGE_SIZE).sum(),
    );
    println!();
    println!("=== Cost gap to #1 (N-best path of the selected surface) ===");
    for (i, &n) in r.gap_hist.iter().enumerate() {
        let lower = i.checked_sub(1).map_or(0, |j| rank_ops::GAP_BIN_UPPER[j]);
        let label = match rank_ops::GAP_BIN_UPPER.get(i) {
            Some(upper) if i == 0 => format!("[0, {upper}]"),
            Some(upper) => format!("({lower}, {upper}]"),
            None => format!("> {lower}"),
        };
        row(&label, n);
    }
    row("no N-best path", r.gap_no_path);
    if r.malformed_lines > 0 {
        println!();
        println!("  Unreadable log lines skipped: {}", r.malformed_lines);
    }
    if let Some(d) = diff {
        println!();
        println!("=== Against baseline ===");
        for (label, n) in [
            ("Lost", d.lost),
            ("Demoted off page", d.demoted_off_page),
            ("Demoted in page", d.demoted_in_page),
            ("Demoted below page", d.demoted_below_page),
            ("Improved", d.improved),
            ("Unchanged", d.unchanged),
            ("Only in current", d.only_current),
            ("Only in baseline", d.only_baseline),
        ] {
            println!("  {:<20} {:>5}", label, n);
        }
    }
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &str) -> Result<Vec<T>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| serde_json::from_str(l).map_err(|e| format!("{path} line {}: {e}", n + 1)))
        .collect()
}

fn write_jsonl<T: Serialize>(path: &str, items: &[T]) -> Result<(), String> {
    let file = fs::File::create(path).map_err(|e| format!("cannot create {path}: {e}"))?;
    let mut w = BufWriter::new(file);
    for item in items {
        let line = serde_json::to_string(item).expect("JSON serialization failed");
        writeln!(w, "{line}").map_err(|e| format!("write {path}: {e}"))?;
    }
    w.flush().map_err(|e| format!("write {path}: {e}"))
}

/// Evaluate one accuracy case. Gates run in order and the first to fail
/// decides the result: no-history `baseline`, top-1 at n=1, then the other
/// display widths and the `[cases.window]` expectations.
fn eval_case(
    dict: &TrieDictionary,
    conn: &ConnectionMatrix,
    hist: Option<&UserHistory>,
    case: &AccuracyCase,
) -> AccuracyResult {
    let base = AccuracyResult {
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
        failure: None,
        details: Vec::new(),
    };
    if case.skip {
        return base;
    }
    let fail = |failure, actual, baseline_actual, details| AccuracyResult {
        status: AccuracyStatus::Fail,
        failure: Some(failure),
        actual,
        baseline_actual,
        details,
        ..base.clone()
    };

    // No-history baseline: every width must still agree on it.
    let plain = case
        .baseline
        .as_ref()
        .map(|_| rank_ops::top1_widths(dict, conn, None, &case.reading));
    let baseline_actual = plain.as_ref().map(|w| w.nbest_head.clone());
    if let (Some(want), Some(w)) = (&case.baseline, &plain) {
        if w.nbest_head != *want {
            return fail(
                Failure::Baseline,
                String::new(),
                baseline_actual,
                Vec::new(),
            );
        }
    }

    let widths = rank_ops::top1_widths(dict, conn, hist, &case.reading);
    let actual = widths.nbest_head.clone();
    if actual != case.expected {
        return fail(Failure::Top1, actual, baseline_actual, Vec::new());
    }

    let want_list_top = case.window_top1.as_deref().unwrap_or(&case.expected);
    let observed = width_disagreements(&widths, &case.expected, want_list_top);
    let (known, mut width): (Vec<String>, Vec<String>) = {
        let (k, u): (Vec<&WidthMismatch>, Vec<&WidthMismatch>) = observed
            .iter()
            .partition(|m| case.width_issue.as_ref().is_some_and(|w| w.covers(m)));
        (
            k.iter().map(|m| m.to_string()).collect(),
            u.iter().map(|m| m.to_string()).collect(),
        )
    };
    // The no-history baseline is never exempt: an exemption names a
    // disagreement under the case's own history.
    if let (Some(want), Some(w)) = (&case.baseline, &plain) {
        width.extend(
            width_disagreements(w, want, want)
                .into_iter()
                .map(|d| format!("baseline {d}")),
        );
    }

    let mut window = Vec::new();
    if let Some(ref w) = case.window {
        window.extend(w.lists.violations(w.n, &widths.list));
        if let Some(ref b) = w.baseline {
            let list = match &plain {
                Some(p) => b.violations(w.n, &p.list),
                None => b.violations(
                    w.n,
                    &rank_ops::production_candidates(dict, conn, None, &case.reading),
                ),
            };
            window.extend(list.into_iter().map(|v| format!("baseline: {v}")));
        }
    }

    let failure = if !window.is_empty() {
        Some(Failure::Window)
    } else if !width.is_empty() {
        Some(Failure::Width)
    } else {
        None
    };
    let (issue, stale) = match &case.width_issue {
        Some(w) => (w.issue.as_str(), w.stale(&observed)),
        None => ("", Vec::new()),
    };
    let details: Vec<String> = window
        .into_iter()
        .chain(width.into_iter().map(|w| format!("width: {w}")))
        .chain(
            known
                .into_iter()
                .map(|w| format!("width ({issue}, known): {w}")),
        )
        .chain(
            stale
                .into_iter()
                .map(|s| format!("width_issue {issue}: {s} — update it")),
        )
        .collect();
    match failure {
        Some(f) => fail(f, actual, baseline_actual, details),
        None => AccuracyResult {
            status: AccuracyStatus::Pass,
            actual,
            baseline_actual,
            details,
            ..base
        },
    }
}

/// Widths other than the n=1 head that disagree with the expected top-1.
fn width_disagreements(
    w: &rank_ops::Top1Widths,
    expected: &str,
    list_top: &str,
) -> Vec<WidthMismatch> {
    let mut out = Vec::new();
    if w.one_best != expected {
        out.push(WidthMismatch::OneBest(w.one_best.clone()));
    }
    if w.list_top() != list_top {
        out.push(WidthMismatch::ListTop {
            got: w.list_top().to_string(),
            want: list_top.to_string(),
        });
    }
    out
}

/// `#123` — the form every skip-like exemption must link (CLAUDE.md).
fn is_issue_ref(s: &str) -> bool {
    s.strip_prefix('#')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn print_audit_text(report: &AuditReport, min_freq: u32, n: usize) {
    if !report.misses.is_empty() {
        println!();
        println!("=== Misses (raw top-1 != your dominant choice) ===");
        for m in &report.misses {
            let rank = match m.rank {
                Some(r) => format!("rank {}", r),
                None => format!("not in top-{}", n),
            };
            let hist = if m.history_fixed {
                "history: fixed"
            } else {
                "history: NOT fixed"
            };
            println!(
                "  \u{2717} {}: you={} \u{00d7}{}, engine={} ({}, {})",
                m.reading, m.dominant, m.frequency, m.raw_top1, rank, hist
            );
        }
    }

    if !report.flip_flops.is_empty() {
        println!();
        println!("=== Flip-flops (multiple surfaces in regular use) ===");
        for f in &report.flip_flops {
            let surfaces: Vec<String> = f
                .surfaces
                .iter()
                .map(|(s, freq)| format!("{} \u{00d7}{}", s, freq))
                .collect();
            println!("  ~ {}: {}", f.reading, surfaces.join(" / "));
        }
    }

    let history_fixed = report.misses.iter().filter(|m| m.history_fixed).count();
    println!();
    println!("=== Summary ===");
    println!(
        "  Readings audited: {} (dominant freq >= {})",
        report.audited, min_freq
    );
    println!(
        "  Raw top-1 agreement: {} ({}/{})",
        report.agree_rate, report.agree, report.audited
    );
    println!(
        "  Misses: {} (history fixes {}, leaves {})",
        report.misses.len(),
        history_fixed,
        report.misses.len() - history_fixed
    );
    println!("  Flip-flops: {}", report.flip_flops.len());
}

fn print_tune_text(result: &tune::TuneResult) {
    let fmt_weights = |w: &tune::FeatureWeights| {
        format!(
            "lv={} te={} sk={}",
            w.length_variance, w.te_kanji, w.single_kanji
        )
    };

    let fmt_rate = |e: &tune::TuneEval| {
        let rate = if e.total > 0 {
            e.pass_count as f64 / e.total as f64 * 100.0
        } else {
            0.0
        };
        format!("{:.1}% ({}/{})", rate, e.pass_count, e.total)
    };

    println!();
    println!("=== Best Weights ===");
    println!("  length_variance: {}", result.best.weights.length_variance);
    println!("  te_kanji:        {}", result.best.weights.te_kanji);
    println!("  single_kanji:    {}", result.best.weights.single_kanji);
    println!("  Pass rate: {}", fmt_rate(&result.best));

    println!();
    println!("=== Default Weights ===");
    println!(
        "  length_variance: {}",
        result.default_eval.weights.length_variance
    );
    println!(
        "  te_kanji:        {}",
        result.default_eval.weights.te_kanji
    );
    println!(
        "  single_kanji:    {}",
        result.default_eval.weights.single_kanji
    );
    println!("  Pass rate: {}", fmt_rate(&result.default_eval));

    if !result.diffs.is_empty() {
        let improvements: Vec<_> = result
            .diffs
            .iter()
            .filter(|d| d.best_pass && !d.default_pass)
            .collect();
        let regressions: Vec<_> = result
            .diffs
            .iter()
            .filter(|d| !d.best_pass && d.default_pass)
            .collect();
        let other: Vec<_> = result
            .diffs
            .iter()
            .filter(|d| d.best_pass == d.default_pass)
            .collect();

        if !improvements.is_empty() {
            println!();
            println!("=== Improvements (default -> best) ===");
            for d in &improvements {
                println!(
                    "  + {}: {} (was: {})",
                    d.reading, d.expected, d.default_top1
                );
            }
        }

        if !regressions.is_empty() {
            println!();
            println!("=== Regressions (default -> best) ===");
            for d in &regressions {
                println!("  - {}: {} -> {}", d.reading, d.expected, d.best_top1);
            }
        }

        if !other.is_empty() {
            println!();
            println!("=== Other changes ===");
            for d in &other {
                println!(
                    "  ~ {}: {} -> {} (expected: {})",
                    d.reading, d.default_top1, d.best_top1, d.expected
                );
            }
        }
    }

    if !result.best_failures.is_empty() {
        println!();
        println!("=== Failures (best weights) ===");
        for f in &result.best_failures {
            println!(
                "  \u{2717} {} \u{2192} {} (got: {})",
                f.reading, f.expected, f.actual
            );
        }
    }

    if result.top_n.len() > 1 {
        println!();
        println!("=== Top {} Weight Combinations ===", result.top_n.len());
        for (i, e) in result.top_n.iter().enumerate() {
            println!(
                "  #{:<2} {}  {}",
                i + 1,
                fmt_rate(e),
                fmt_weights(&e.weights)
            );
        }
    }
}

fn print_tune_json(result: &tune::TuneResult) {
    let weight_json = |w: &tune::FeatureWeights| -> serde_json::Value {
        serde_json::json!({
            "structure": w.structure,
            "length_variance": w.length_variance,
            "te_kanji": w.te_kanji,
            "single_kanji": w.single_kanji,
            "script": w.script,
        })
    };

    let eval_json = |e: &tune::TuneEval| -> serde_json::Value {
        let rate = if e.total > 0 {
            e.pass_count as f64 / e.total as f64 * 100.0
        } else {
            0.0
        };
        serde_json::json!({
            "weights": weight_json(&e.weights),
            "pass_count": e.pass_count,
            "total": e.total,
            "pass_rate": format!("{:.1}%", rate),
        })
    };

    let diffs: Vec<serde_json::Value> = result
        .diffs
        .iter()
        .map(|d| {
            serde_json::json!({
                "reading": d.reading,
                "expected": d.expected,
                "default_top1": d.default_top1,
                "best_top1": d.best_top1,
                "default_pass": d.default_pass,
                "best_pass": d.best_pass,
            })
        })
        .collect();

    let top_n: Vec<serde_json::Value> = result.top_n.iter().map(eval_json).collect();

    let report = serde_json::json!({
        "best": eval_json(&result.best),
        "default": eval_json(&result.default_eval),
        "diffs": diffs,
        "top_n": top_n,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("JSON serialization failed")
    );
}
