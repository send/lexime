//! Rank-2+ measurement: production-width candidate lists, top-1 at every
//! display width, `[cases.window]` checks for the accuracy corpora, and
//! commit-log replay.
//!
//! The commit log holds the user's personal input. Replay therefore returns
//! counts ([`ReplayReport`]) and baseline lines ([`BaselineLine`]: line index,
//! timestamp, rank) — never a reading or surface. The only way content leaves
//! this module is the explicit `verbose` flag, which writes to stderr for
//! local inspection.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};

use lex_core::candidates::{generate_candidates_priced, CandidateResponse, PricedCandidates};
use lex_core::converter::{
    convert_nbest, convert_nbest_with_history, ConversionContext, ConvertedSegment,
};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::Dictionary;
use lex_core::settings::settings;
use lex_core::user_history::UserHistory;

/// Candidates shown on the first page of the candidate window. Mirrors
/// `CandidateManager.maxDisplay` in Sources/CandidateManager.swift, which
/// owns the value; change both together.
pub const PAGE_SIZE: usize = 9;

/// Concatenated surface of a conversion path.
pub fn joined_surface(segments: &[ConvertedSegment]) -> String {
    segments.iter().map(|s| s.surface.as_str()).collect()
}

/// The candidate list as the IME builds it: N-best + learned injection +
/// kana + predictions + lookup, at `candidates.max_results` — the limit the
/// async worker uses. (The session's synchronous path uses its own
/// `MAX_CANDIDATES`; both are 20.)
///
/// Callers slice the result with `take(n)`; passing a smaller limit would
/// change which predictions are fetched for short readings. This is the
/// Standard conversion mode's list; Predictive mode builds a different one.
pub fn production_candidates(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    history: Option<&UserHistory>,
    reading: &str,
) -> CandidateResponse {
    production_candidates_priced(dict, conn, history, reading).response
}

/// [`production_candidates`] with each N-best path's final cost, from the
/// same run — so a cost is always the price of the path the list shows.
pub fn production_candidates_priced(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    history: Option<&UserHistory>,
    reading: &str,
) -> PricedCandidates {
    generate_candidates_priced(
        dict,
        Some(conn),
        history,
        reading,
        settings().candidates.max_results,
    )
}

/// Top-1 at each width the user can see. They oversample differently, so
/// the reranker's structure filter sees different populations and the
/// top-1 can diverge between them.
pub struct Top1Widths {
    /// N-best head at n=1 — the historical accuracy gate.
    pub nbest_head: String,
    /// Synchronous 1-best shown while candidates are pending (the session's
    /// deferred-candidates response).
    pub one_best: String,
    /// The production candidate list, whose #1 is `list.surfaces[0]`.
    pub list: CandidateResponse,
}

impl Top1Widths {
    pub fn list_top(&self) -> &str {
        self.list.surfaces.first().map_or("", String::as_str)
    }
}

pub fn top1_widths(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    history: Option<&UserHistory>,
    reading: &str,
) -> Top1Widths {
    let head = match history {
        Some(h) => convert_nbest_with_history(dict, Some(conn), h, reading, 1),
        None => convert_nbest(dict, Some(conn), reading, 1),
    };
    let ctx = ConversionContext {
        dict,
        conn: Some(conn),
        history,
    };
    Top1Widths {
        nbest_head: head.first().map(|p| joined_surface(p)).unwrap_or_default(),
        one_best: joined_surface(&ctx.convert_from_lattice(&ctx.build_lattice(reading))),
        list: production_candidates(dict, conn, history, reading),
    }
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
    #[serde(flatten)]
    pub lists: WindowLists,
    /// History corpora only (and required there): the same checks without
    /// history, like `baseline` for top-1, so a learning effect is shown
    /// rather than assumed.
    pub baseline: Option<WindowLists>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowLists {
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
}

impl WindowCheck {
    /// Reject checks that can never be meaningful. `history_corpus` is true
    /// when the corpus is evaluated with history.
    pub fn validate(&self, history_corpus: bool) -> Result<(), String> {
        if self.n == 0 {
            return Err("window.n must be at least 1".into());
        }
        match (history_corpus, &self.baseline) {
            (true, None) => {
                return Err("a window in a history corpus needs [cases.window.baseline]".into())
            }
            (false, Some(_)) => {
                return Err("[cases.window.baseline] is only meaningful with history".into())
            }
            _ => {}
        }
        self.lists.check_consistent()?;
        if let Some(b) = &self.baseline {
            b.check_consistent().map_err(|e| format!("baseline: {e}"))?;
        }
        Ok(())
    }
}

impl WindowLists {
    fn check_consistent(&self) -> Result<(), String> {
        match self
            .present
            .iter()
            .chain(&self.present_nbest)
            .find(|s| self.absent.contains(s))
        {
            Some(s) => Err(format!("{s} is both required and forbidden")),
            None => Ok(()),
        }
    }

    /// Human-readable violations against the first `n` of `resp`.
    pub fn violations(&self, n: usize, resp: &CandidateResponse) -> Vec<String> {
        let window: Vec<&str> = resp.surfaces.iter().take(n).map(String::as_str).collect();
        let in_nbest = |s: &str| resp.paths.iter().any(|p| joined_surface(p) == s);
        let mut out = Vec::new();
        for s in self.present.iter().chain(&self.present_nbest) {
            if !window.contains(&s.as_str()) {
                out.push(format!("{s} not in top {n}"));
            }
        }
        for s in &self.present_nbest {
            if window.contains(&s.as_str()) && !in_nbest(s) {
                out.push(format!("{s} in top {n} but not from an N-best path"));
            }
        }
        for s in &self.absent {
            if let Some(i) = window.iter().position(|w| w == s) {
                out.push(format!("{s} at rank {} (must be outside top {n})", i + 1));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Commit-log replay
// ---------------------------------------------------------------------------

/// The fields of a commit-log line replay needs. The contract is SPEC
/// §コミットログ; the writer is `commit_log_line` in the engine crate,
/// which lex-cli cannot depend on. Content stays private to this module.
#[derive(Deserialize)]
struct CommitLine {
    t: u64,
    reading: String,
    surface: String,
    rank: usize,
}

/// The rank>0 lines of a commit log, with their 0-based line index.
struct Selections {
    lines: Vec<(usize, CommitLine)>,
    /// Lines that are not valid UTF-8 JSON of the expected shape — e.g. a
    /// torn last line while the IME is appending. Counted, never echoed.
    malformed: usize,
}

fn read_selections(path: &Path) -> Result<Selections, String> {
    let file = fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut out = Selections {
        lines: Vec::new(),
        malformed: 0,
    };
    let mut buf = Vec::new();
    for i in 0.. {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("read error at line {}: {e}", i + 1))?;
        if n == 0 {
            break;
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            out.malformed += 1;
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<CommitLine>(line) {
            Ok(rec) if rec.rank > 0 => out.lines.push((i, rec)),
            Ok(_) => {}
            Err(_) => out.malformed += 1,
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
#[derive(Debug, Serialize)]
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
    /// Log lines skipped as unreadable (see `Selections::malformed`).
    pub malformed_lines: usize,
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
    /// Off the first page before, and moved further down.
    pub demoted_below_page: usize,
    /// Moved up, or absent before and present now.
    pub improved: usize,
    pub unchanged: usize,
    /// Selections with no baseline line (log grew or was rewritten).
    pub only_current: usize,
    /// Baseline lines with no current selection.
    pub only_baseline: usize,
}

/// What replay needs per distinct reading, computed once.
struct ReadingView {
    /// The production candidate list.
    surfaces: Vec<String>,
    /// The list's N-best paths as (surface, final cost), cheapest first —
    /// from the same run as `surfaces`.
    costs: Vec<(String, i64)>,
}

/// Replay every rank>0 selection in `log`. Returns the counts and one
/// baseline line per selection (for `--emit-baseline` / `--baseline`).
/// A log with no rank>0 selection is a valid, all-zero measurement.
/// `verbose` prints per-selection content to stderr (local inspection only).
pub fn replay(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    history: Option<&UserHistory>,
    log: &Path,
    verbose: bool,
) -> Result<(ReplayReport, Vec<BaselineLine>), String> {
    let Selections {
        lines: selections,
        malformed,
    } = read_selections(log)?;

    let mut rank_hist = vec![0; PAGE_SIZE];
    let mut gap_hist = vec![0; GAP_BIN_UPPER.len() + 1];
    let mut gap_no_path = 0;
    let mut lines = Vec::with_capacity(selections.len());
    let mut cache: HashMap<&str, ReadingView> = HashMap::new();

    for (i, sel) in &selections {
        let ReadingView { surfaces, costs } =
            cache.entry(sel.reading.as_str()).or_insert_with(|| {
                let PricedCandidates {
                    response,
                    path_costs,
                } = production_candidates_priced(dict, conn, history, &sel.reading);
                ReadingView {
                    costs: response
                        .paths
                        .iter()
                        .map(|p| joined_surface(p))
                        .zip(path_costs)
                        .collect(),
                    surfaces: response.surfaces,
                }
            });
        let rank = surfaces.iter().position(|s| *s == sel.surface);
        let gap = costs.first().and_then(|(_, top)| {
            costs
                .iter()
                .find(|(s, _)| *s == sel.surface)
                .map(|(_, c)| c - top)
        });

        if let Some(r) = rank {
            count_rank(&mut rank_hist, r);
        }
        match gap {
            Some(g) => gap_hist[gap_bin(g)] += 1,
            None => gap_no_path += 1,
        }
        if verbose {
            eprintln!(
                "{i}\t{}\t{}\trank={}\tgap={}",
                sel.reading,
                sel.surface,
                rank.map_or("-".into(), |r| r.to_string()),
                gap.map_or("-".into(), |g| g.to_string()),
            );
        }
        lines.push(BaselineLine {
            i: *i,
            t: sel.t,
            rank,
        });
    }

    let in_list: usize = rank_hist.iter().sum();
    let report = ReplayReport {
        selections: selections.len(),
        in_page: rank_hist.iter().take(PAGE_SIZE).sum(),
        in_list,
        absent: selections.len() - in_list,
        rank_hist,
        gap_hist,
        gap_no_path,
        malformed_lines: malformed,
    };
    Ok((report, lines))
}

fn count_rank(hist: &mut Vec<usize>, rank: usize) {
    if hist.len() <= rank {
        hist.resize(rank + 1, 0);
    }
    hist[rank] += 1;
}

/// Compare ranks per selection, joined on `(i, t)`: a cleared and rewritten
/// log reuses line indices, so the index alone would mis-join. `t` has
/// whole-second resolution, so a rewritten line at the same index within the
/// same second as the old one still mis-joins — that takes several commits a
/// second on both sides of the clear. A content-derived key would close it
/// but would put personal input into the baseline file.
pub fn diff_baseline(before: &[BaselineLine], after: &[BaselineLine]) -> BaselineDiff {
    let before_map: HashMap<(usize, u64), Option<usize>> =
        before.iter().map(|b| ((b.i, b.t), b.rank)).collect();
    let mut d = BaselineDiff::default();
    let mut matched = 0;
    for a in after {
        let Some(&was) = before_map.get(&(a.i, a.t)) else {
            d.only_current += 1;
            continue;
        };
        matched += 1;
        match (was, a.rank) {
            (Some(_), None) => d.lost += 1,
            (None, Some(_)) => d.improved += 1,
            (None, None) => d.unchanged += 1,
            (Some(b), Some(r)) if r == b => d.unchanged += 1,
            (Some(b), Some(r)) if r < b => d.improved += 1,
            (Some(b), Some(_)) if b >= PAGE_SIZE => d.demoted_below_page += 1,
            (Some(_), Some(r)) if r >= PAGE_SIZE => d.demoted_off_page += 1,
            (Some(_), Some(_)) => d.demoted_in_page += 1,
        }
    }
    d.only_baseline = before_map.len() - matched;
    d
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn window_present_absent_and_slicing() {
        let w = check(
            r#"n = 2
present = ["b"]
absent = ["c"]"#,
        );
        assert!(w
            .lists
            .violations(w.n, &resp(&["a", "b", "c"], &[]))
            .is_empty());
        let v = w.lists.violations(w.n, &resp(&["a", "c", "b"], &[]));
        assert_eq!(v.len(), 2, "{v:?}");
        // Fewer candidates than n must not panic.
        assert_eq!(w.lists.violations(w.n, &resp(&["a"], &[])).len(), 1);
    }

    #[test]
    fn window_present_nbest_rejects_injected_surface() {
        let w = check(
            r#"n = 3
present_nbest = ["b"]"#,
        );
        assert!(w
            .lists
            .violations(w.n, &resp(&["a", "b"], &["a", "b"]))
            .is_empty());
        let v = w.lists.violations(w.n, &resp(&["a", "b"], &["a"]));
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
        // The baseline table is required with history, forbidden without.
        assert!(check("n = 9").validate(true).is_err());
        let with_base = check("n = 9\n[baseline]\nabsent = [\"x\"]");
        assert!(with_base.validate(true).is_ok());
        assert!(with_base.validate(false).is_err());
        let clash = check("n = 9\n[baseline]\npresent = [\"x\"]\nabsent = [\"x\"]");
        assert!(clash.validate(true).is_err());
        assert!(toml::from_str::<WindowCheck>("n = 9\nbogus = 1").is_err());
    }

    #[test]
    fn baseline_lists_are_checked_separately() {
        let w = check("n = 2\npresent = [\"x\"]\n[baseline]\nabsent = [\"x\"]");
        let r = resp(&["a", "x"], &[]);
        assert!(w.lists.violations(w.n, &r).is_empty());
        assert_eq!(w.baseline.as_ref().unwrap().violations(w.n, &r).len(), 1);
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
            line(0, 10, Some(1)),  // lost
            line(1, 11, Some(2)),  // off page
            line(2, 12, Some(1)),  // down in page
            line(3, 13, Some(5)),  // up
            line(4, 14, None),     // appears
            line(5, 15, Some(3)),  // same
            line(6, 16, Some(1)),  // gone from log
            line(7, 17, Some(12)), // further below page 1
        ];
        let after = [
            line(0, 10, None),
            line(1, 11, Some(PAGE_SIZE)),
            line(2, 12, Some(4)),
            line(3, 13, Some(2)),
            line(4, 14, Some(7)),
            line(5, 15, Some(3)),
            line(7, 17, Some(15)),
            line(9, 99, Some(1)), // new line
        ];
        assert_eq!(
            diff_baseline(&before, &after),
            BaselineDiff {
                lost: 1,
                demoted_off_page: 1,
                demoted_in_page: 1,
                demoted_below_page: 1,
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
    fn unreadable_commit_log_lines_are_counted_not_fatal() {
        let dir = std::env::temp_dir().join(format!("lexcli-replay-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("commit-log.jsonl");
        let mut content = Vec::new();
        content.extend_from_slice(
            "{\"t\":1,\"reading\":\"あ\",\"surface\":\"亜\",\"rank\":0}\n".as_bytes(),
        );
        content.extend_from_slice(
            "{\"t\":2,\"reading\":\"い\",\"surface\":\"胃\",\"rank\":2,\"auto\":true}\n".as_bytes(),
        );
        content.extend_from_slice(b"not json\n");
        // A torn multibyte character, as a crash mid-append would leave.
        content.extend_from_slice(b"{\"t\":3,\"reading\":\"\xe3\x81");
        fs::write(&path, &content).unwrap();

        let sels = read_selections(&path).unwrap();
        assert_eq!(sels.malformed, 2);
        assert_eq!(sels.lines.len(), 1, "only rank>0 lines are replayed");
        assert_eq!((sels.lines[0].0, sels.lines[0].1.t), (1, 2));

        // No rank>0 line at all is an empty measurement, not an error.
        fs::write(
            &path,
            "{\"t\":1,\"reading\":\"あ\",\"surface\":\"亜\",\"rank\":0}\n",
        )
        .unwrap();
        let sels = read_selections(&path).unwrap();
        assert!(sels.lines.is_empty());
        assert_eq!(sels.malformed, 0);
        fs::remove_dir_all(&dir).ok();
    }

    /// PAGE_SIZE mirrors the Swift constant that owns it. Fail closed: if the
    /// declaration moves or changes shape, this test fails and PAGE_SIZE is
    /// re-checked by hand.
    #[test]
    fn page_size_matches_swift_candidate_window() {
        let swift =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Sources/CandidateManager.swift");
        let src = fs::read_to_string(&swift).expect("read Sources/CandidateManager.swift");
        let decl = format!("static let maxDisplay = {PAGE_SIZE}\n");
        assert!(
            src.contains(&decl),
            "CandidateManager.maxDisplay no longer reads `{}`; update PAGE_SIZE",
            decl.trim()
        );
    }
}
