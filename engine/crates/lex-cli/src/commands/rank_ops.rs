//! Rank-2+ measurement: production-width candidate lists, `[cases.window]`
//! checks for the accuracy corpora, and commit-log replay.
//!
//! The commit log holds the user's personal input. Replay therefore returns
//! counts only ([`ReplayReport`]); the per-line baseline it can emit carries
//! a line index, timestamp and rank — never a reading or surface. The only
//! way content leaves this module is the explicit `verbose` flag, which
//! writes to stderr for local inspection.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use lex_core::candidates::{generate_candidates, CandidateResponse};
use lex_core::converter::explain;
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::Dictionary;
use lex_core::settings::settings;
use lex_core::user_history::UserHistory;

/// Candidates shown on the first page of the candidate window.
pub const PAGE_SIZE: usize = 9;

/// The candidate list exactly as the IME builds it: N-best + learned
/// injection + kana + predictions + lookup, at the production result limit.
///
/// Callers slice the result with `take(n)`; passing a smaller `max_results`
/// would change which predictions are fetched for short readings.
pub fn production_candidates(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    history: Option<&UserHistory>,
    reading: &str,
) -> CandidateResponse {
    generate_candidates(
        dict,
        Some(conn),
        history,
        reading,
        settings().candidates.max_results,
    )
}

// ---------------------------------------------------------------------------
// [cases.window]
// ---------------------------------------------------------------------------

/// Rank-2+ expectations for one accuracy case, checked against the first `n`
/// production candidates.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowCheck {
    pub n: usize,
    /// Must appear in the first `n` candidates.
    #[serde(default)]
    pub present: Vec<String>,
    /// Must appear in the first `n` candidates AND come from an N-best path,
    /// so a surface re-injected from history cannot satisfy it.
    #[serde(default)]
    pub present_nbest: Vec<String>,
    /// Must not appear in the first `n` candidates.
    #[serde(default)]
    pub absent: Vec<String>,
    /// History corpora only: the same checks without history, mirroring the
    /// top-1 `baseline` so a learning effect is shown, not assumed.
    pub baseline_present: Option<Vec<String>>,
    pub baseline_present_nbest: Option<Vec<String>>,
    pub baseline_absent: Option<Vec<String>>,
}

impl WindowCheck {
    /// Reject checks that can never be meaningful. `history_corpus` is true
    /// when the corpus is evaluated with history.
    pub fn validate(&self, history_corpus: bool) -> Result<(), String> {
        if self.n == 0 {
            return Err("window.n must be at least 1".into());
        }
        let baselines = [
            &self.baseline_present,
            &self.baseline_present_nbest,
            &self.baseline_absent,
        ];
        if history_corpus {
            if baselines.iter().any(|b| b.is_none()) {
                return Err("a window in a history corpus needs baseline_present, \
                     baseline_present_nbest and baseline_absent (use [] for none)"
                    .into());
            }
        } else if baselines.iter().any(|b| b.is_some()) {
            return Err("baseline_* window fields are only meaningful with history".into());
        }
        let clash = |present: &[String], nbest: &[String], absent: &[String]| {
            present
                .iter()
                .chain(nbest)
                .find(|s| absent.contains(s))
                .cloned()
        };
        if let Some(s) = clash(&self.present, &self.present_nbest, &self.absent) {
            return Err(format!("{s} is both required and forbidden"));
        }
        if let (Some(p), Some(pn), Some(a)) = (
            &self.baseline_present,
            &self.baseline_present_nbest,
            &self.baseline_absent,
        ) {
            if let Some(s) = clash(p, pn, a) {
                return Err(format!(
                    "{s} is both required and forbidden in the baseline"
                ));
            }
        }
        Ok(())
    }

    /// Violations of the with-history (or only) expectations.
    pub fn violations(&self, resp: &CandidateResponse) -> Vec<String> {
        window_violations(
            self.n,
            &self.present,
            &self.present_nbest,
            &self.absent,
            resp,
        )
    }

    /// Violations of the no-history baseline expectations, if any are set.
    pub fn baseline_violations(&self, resp: &CandidateResponse) -> Vec<String> {
        let empty = Vec::new();
        window_violations(
            self.n,
            self.baseline_present.as_ref().unwrap_or(&empty),
            self.baseline_present_nbest.as_ref().unwrap_or(&empty),
            self.baseline_absent.as_ref().unwrap_or(&empty),
            resp,
        )
    }

    pub fn has_baseline(&self) -> bool {
        self.baseline_present.is_some()
    }
}

fn window_violations(
    n: usize,
    present: &[String],
    present_nbest: &[String],
    absent: &[String],
    resp: &CandidateResponse,
) -> Vec<String> {
    let window: Vec<&str> = resp.surfaces.iter().take(n).map(String::as_str).collect();
    let nbest: HashSet<String> = resp
        .paths
        .iter()
        .map(|p| p.iter().map(|s| s.surface.as_str()).collect())
        .collect();
    let mut out = Vec::new();
    for s in present {
        if !window.contains(&s.as_str()) {
            out.push(format!("{s} not in top {n}"));
        }
    }
    for s in present_nbest {
        if !window.contains(&s.as_str()) {
            out.push(format!("{s} not in top {n}"));
        } else if !nbest.contains(s) {
            out.push(format!("{s} in top {n} but not from an N-best path"));
        }
    }
    for s in absent {
        if let Some(i) = window.iter().position(|w| w == s) {
            out.push(format!("{s} at rank {} (must be outside top {n})", i + 1));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Commit-log replay
// ---------------------------------------------------------------------------

/// One rank>0 selection from the commit log. Content stays private to this
/// module; see the module docs.
struct Selection {
    /// 0-based line index in the commit log.
    i: usize,
    t: u64,
    reading: String,
    surface: String,
}

/// The fields of a commit-log line replay needs. The contract is SPEC
/// §コミットログ; the writer is `commit_log_line` in the engine crate,
/// which lex-cli cannot depend on.
#[derive(Deserialize)]
struct CommitLine {
    t: u64,
    reading: String,
    surface: String,
    rank: usize,
}

fn read_selections(path: &Path) -> Result<Vec<Selection>, String> {
    let file = fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("read error at line {}: {e}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        // The error must not echo the line: it is personal content.
        let rec: CommitLine = serde_json::from_str(&line)
            .map_err(|_| format!("malformed commit-log line {}", i + 1))?;
        if rec.rank > 0 {
            out.push(Selection {
                i,
                t: rec.t,
                reading: rec.reading,
                surface: rec.surface,
            });
        }
    }
    Ok(out)
}

/// A baseline line: identifies a selection by position and time only.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BaselineLine {
    pub i: usize,
    pub t: u64,
    /// 0-based rank in the production list; `None` = absent.
    pub rank: Option<usize>,
}

/// Upper bounds (inclusive) of the cost-gap bins; the last bin is open.
pub const GAP_BIN_UPPER: [i64; 6] = [2000, 4000, 6000, 8000, 10000, 12000];

fn gap_bin(gap: i64) -> usize {
    GAP_BIN_UPPER
        .iter()
        .position(|&upper| gap <= upper)
        .unwrap_or(GAP_BIN_UPPER.len())
}

/// Counts-only replay result. Adding a field here is the only way to
/// publish more; keep it free of readings and surfaces.
#[derive(Debug, Default, Serialize)]
pub struct ReplayReport {
    /// rank>0 selections replayed.
    pub selections: usize,
    /// Present on the first page (rank < PAGE_SIZE).
    pub in_page: usize,
    /// Present anywhere in the production list.
    pub in_list: usize,
    pub absent: usize,
    /// `rank_hist[r]` = selections now at 0-based rank r. The list can run
    /// past `max_results` (lookup entries are appended), so this grows to the
    /// deepest rank seen.
    pub rank_hist: Vec<usize>,
    /// Cost gap of the selected surface's N-best path to the #1 path,
    /// binned by [`GAP_BIN_UPPER`] plus an open last bin.
    pub gap_hist: Vec<usize>,
    /// Selections with no N-best path (lookup / prediction / injection only).
    pub gap_no_path: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineDiff>,
}

/// Movement against a baseline, joined on `(i, t)`.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct BaselineDiff {
    /// Present before, absent now.
    pub lost: usize,
    /// On the first page before, present but off it now.
    pub demoted_off_page: usize,
    /// Moved down but still on the first page.
    pub demoted_in_page: usize,
    /// Moved up, or absent before and present now.
    pub improved: usize,
    pub unchanged: usize,
    /// Selections with no baseline line (log grew or was rewritten).
    pub only_current: usize,
    /// Baseline lines with no current selection.
    pub only_baseline: usize,
}

pub struct ReplayOptions<'a> {
    pub history: Option<&'a UserHistory>,
    pub baseline: Option<&'a Path>,
    pub emit_baseline: Option<&'a Path>,
    /// Print per-selection content to stderr (local inspection only).
    pub verbose: bool,
}

pub fn replay(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    log: &Path,
    opts: &ReplayOptions<'_>,
) -> Result<ReplayReport, String> {
    let selections = read_selections(log)?;
    if selections.is_empty() {
        return Err(format!("no rank>0 selections in {}", log.display()));
    }
    let nbest = settings().candidates.nbest;

    let mut report = ReplayReport {
        rank_hist: vec![0; PAGE_SIZE],
        gap_hist: vec![0; GAP_BIN_UPPER.len() + 1],
        ..Default::default()
    };
    let mut lines = Vec::with_capacity(selections.len());
    let mut surfaces_cache: HashMap<&str, Vec<String>> = HashMap::new();
    let mut costs_cache: HashMap<&str, Vec<(String, i64)>> = HashMap::new();

    for sel in &selections {
        let surfaces = surfaces_cache
            .entry(sel.reading.as_str())
            .or_insert_with(|| {
                production_candidates(dict, conn, opts.history, &sel.reading).surfaces
            });
        let rank = surfaces.iter().position(|s| *s == sel.surface);

        let costs = costs_cache.entry(sel.reading.as_str()).or_insert_with(|| {
            explain::explain(dict, Some(conn), opts.history, &sel.reading, nbest)
                .paths
                .iter()
                .map(|p| (p.surface(), p.final_cost))
                .collect()
        });
        let top = costs.first().map(|(_, c)| *c);
        let gap = costs
            .iter()
            .find(|(s, _)| *s == sel.surface)
            .zip(top)
            .map(|((_, c), top)| c - top);

        report.selections += 1;
        match rank {
            Some(r) => {
                report.in_list += 1;
                if r < PAGE_SIZE {
                    report.in_page += 1;
                }
                count_rank(&mut report.rank_hist, r);
            }
            None => report.absent += 1,
        }
        match gap {
            Some(g) => report.gap_hist[gap_bin(g)] += 1,
            None => report.gap_no_path += 1,
        }
        if opts.verbose {
            eprintln!(
                "{}\t{}\t{}\trank={}\tgap={}",
                sel.i,
                sel.reading,
                sel.surface,
                rank.map_or("-".into(), |r| r.to_string()),
                gap.map_or("-".into(), |g| g.to_string()),
            );
        }
        lines.push(BaselineLine {
            i: sel.i,
            t: sel.t,
            rank,
        });
    }

    if let Some(path) = opts.baseline {
        let before = read_baseline(path)?;
        report.baseline = Some(diff_baseline(&before, &lines));
    }
    if let Some(path) = opts.emit_baseline {
        write_baseline(path, &lines)?;
    }
    Ok(report)
}

fn count_rank(hist: &mut Vec<usize>, rank: usize) {
    if hist.len() <= rank {
        hist.resize(rank + 1, 0);
    }
    hist[rank] += 1;
}

fn read_baseline(path: &Path) -> Result<Vec<BaselineLine>, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| serde_json::from_str(l).map_err(|e| format!("baseline line {}: {e}", n + 1)))
        .collect()
}

fn write_baseline(path: &Path, lines: &[BaselineLine]) -> Result<(), String> {
    let mut f =
        fs::File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    for line in lines {
        let json = serde_json::to_string(line).expect("BaselineLine serializes");
        writeln!(f, "{json}").map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Compare ranks per selection, joined on `(i, t)`: a cleared and rewritten
/// log reuses line indices, so the index alone would mis-join.
pub fn diff_baseline(before: &[BaselineLine], after: &[BaselineLine]) -> BaselineDiff {
    let before_map: HashMap<(usize, u64), Option<usize>> =
        before.iter().map(|b| ((b.i, b.t), b.rank)).collect();
    let after_keys: HashSet<(usize, u64)> = after.iter().map(|a| (a.i, a.t)).collect();
    let mut d = BaselineDiff {
        only_baseline: before_map
            .keys()
            .filter(|k| !after_keys.contains(k))
            .count(),
        ..Default::default()
    };
    for a in after {
        let Some(&was) = before_map.get(&(a.i, a.t)) else {
            d.only_current += 1;
            continue;
        };
        match (was, a.rank) {
            (Some(_), None) => d.lost += 1,
            (None, Some(_)) => d.improved += 1,
            (None, None) => d.unchanged += 1,
            (Some(b), Some(r)) if r == b => d.unchanged += 1,
            (Some(b), Some(r)) if r < b => d.improved += 1,
            (Some(b), Some(r)) if b < PAGE_SIZE && r >= PAGE_SIZE => d.demoted_off_page += 1,
            (Some(_), Some(_)) => d.demoted_in_page += 1,
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use lex_core::converter::ConvertedSegment;

    fn resp(surfaces: &[&str], nbest: &[&str]) -> CandidateResponse {
        CandidateResponse {
            surfaces: surfaces.iter().map(|s| s.to_string()).collect(),
            paths: nbest
                .iter()
                .map(|s| {
                    vec![ConvertedSegment {
                        reading: String::new(),
                        surface: s.to_string(),
                    }]
                })
                .collect(),
        }
    }

    fn check(toml_src: &str) -> WindowCheck {
        toml::from_str(toml_src).expect("valid window")
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn window_present_absent_and_slicing() {
        let w = check(
            r#"n = 2
present = ["b"]
absent = ["c"]"#,
        );
        assert!(w.violations(&resp(&["a", "b", "c"], &[])).is_empty());
        let v = w.violations(&resp(&["a", "c", "b"], &[]));
        assert_eq!(v.len(), 2, "{v:?}");
        // Fewer candidates than n must not panic.
        assert_eq!(w.violations(&resp(&["a"], &[])).len(), 1);
    }

    #[test]
    fn window_present_nbest_rejects_injected_surface() {
        let w = check(
            r#"n = 3
present_nbest = ["b"]"#,
        );
        assert!(w.violations(&resp(&["a", "b"], &["a", "b"])).is_empty());
        let v = w.violations(&resp(&["a", "b"], &["a"]));
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("not from an N-best path"));
    }

    #[test]
    fn window_validation() {
        assert!(check("n = 0").validate(false).is_err());
        assert!(check(
            r#"n = 9
present = ["x"]
absent = ["x"]"#
        )
        .validate(false)
        .is_err());
        // baseline_* required with history, forbidden without.
        assert!(check("n = 9").validate(true).is_err());
        let with_base = check(
            r#"n = 9
baseline_present = []
baseline_present_nbest = []
baseline_absent = []"#,
        );
        assert!(with_base.validate(true).is_ok());
        assert!(with_base.validate(false).is_err());
        assert!(toml::from_str::<WindowCheck>("n = 9\nbogus = 1").is_err());
    }

    #[test]
    fn baseline_violations_use_baseline_lists() {
        let w = WindowCheck {
            n: 2,
            present: strs(&["x"]),
            present_nbest: vec![],
            absent: vec![],
            baseline_present: Some(vec![]),
            baseline_present_nbest: Some(vec![]),
            baseline_absent: Some(strs(&["x"])),
        };
        let r = resp(&["a", "x"], &[]);
        assert!(w.violations(&r).is_empty());
        assert_eq!(w.baseline_violations(&r).len(), 1);
    }

    #[test]
    fn gap_bins_are_inclusive_upper() {
        assert_eq!(gap_bin(0), 0);
        assert_eq!(gap_bin(2000), 0);
        assert_eq!(gap_bin(2001), 1);
        assert_eq!(gap_bin(12000), 5);
        assert_eq!(gap_bin(12001), 6);
    }

    #[test]
    fn rank_hist_grows_past_its_initial_size() {
        let mut hist = vec![0; PAGE_SIZE];
        count_rank(&mut hist, 30);
        count_rank(&mut hist, 2);
        assert_eq!(hist.len(), 31);
        assert_eq!(hist.iter().sum::<usize>(), 2);
        assert_eq!(hist[30], 1);
    }

    fn line(i: usize, t: u64, rank: Option<usize>) -> BaselineLine {
        BaselineLine { i, t, rank }
    }

    #[test]
    fn baseline_diff_classifies_moves() {
        let before = [
            line(0, 10, Some(1)), // lost
            line(1, 11, Some(2)), // off page
            line(2, 12, Some(1)), // down in page
            line(3, 13, Some(5)), // up
            line(4, 14, None),    // appears
            line(5, 15, Some(3)), // same
            line(6, 16, Some(1)), // gone from log
        ];
        let after = [
            line(0, 10, None),
            line(1, 11, Some(PAGE_SIZE)),
            line(2, 12, Some(4)),
            line(3, 13, Some(2)),
            line(4, 14, Some(7)),
            line(5, 15, Some(3)),
            line(9, 99, Some(1)), // new line
        ];
        assert_eq!(
            diff_baseline(&before, &after),
            BaselineDiff {
                lost: 1,
                demoted_off_page: 1,
                demoted_in_page: 1,
                improved: 2,
                unchanged: 1,
                only_current: 1,
                only_baseline: 1,
            }
        );
    }

    #[test]
    fn baseline_join_needs_matching_timestamp() {
        // Same index, different time = the log was cleared and rewritten.
        let d = diff_baseline(&[line(0, 10, Some(1))], &[line(0, 20, None)]);
        assert_eq!(d.lost, 0);
        assert_eq!(d.only_current, 1);
        assert_eq!(d.only_baseline, 1);
    }

    #[test]
    fn commit_log_parse_errors_do_not_echo_content() {
        let dir = std::env::temp_dir().join(format!("lexcli-replay-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("commit-log.jsonl");
        fs::write(
            &path,
            "{\"t\":1,\"reading\":\"あ\",\"surface\":\"亜\",\"rank\":0}\n\
             {\"t\":2,\"reading\":\"い\",\"surface\":\"胃\",\"rank\":2,\"top1\":\"い\"}\n\
             not json with ひみつ\n",
        )
        .unwrap();
        let err = read_selections(&path).err().expect("malformed line");
        assert!(!err.contains("ひみつ"), "{err}");
        assert!(err.contains("line 3"), "{err}");

        fs::write(
            &path,
            "{\"t\":1,\"reading\":\"あ\",\"surface\":\"亜\",\"rank\":0}\n\
             {\"t\":2,\"reading\":\"い\",\"surface\":\"胃\",\"rank\":2,\"auto\":true}\n",
        )
        .unwrap();
        let sels = read_selections(&path).unwrap();
        assert_eq!(sels.len(), 1, "only rank>0 lines are replayed");
        assert_eq!((sels[0].i, sels[0].t), (1, 2));
        fs::remove_dir_all(&dir).ok();
    }
}
