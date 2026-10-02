//! Rank-2+ measurement: production-width candidate lists, top-1 at every
//! display width, `[cases.window]` checks for the accuracy corpora, and
//! commit-log replay.
//!
//! The commit log holds the user's personal input. Replay therefore returns
//! counts ([`ReplayReport`]) and baseline lines ([`BaselineLine`]: line index,
//! timestamp, rank) — never a reading or surface. The only way content leaves
//! this module is the explicit `verbose` flag, which writes to stderr for
//! local inspection.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};

use lex_core::candidates::{
    generate_candidates, generate_candidates_priced, CandidateResponse, PathPrice, PricedCandidates,
};
use lex_core::converter::{
    convert_nbest, convert_nbest_with_history, ConversionContext, ConvertedSegment, PathOrigin,
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
    generate_candidates(
        dict,
        Some(conn),
        history,
        reading,
        settings().candidates.max_results,
    )
}

/// [`production_candidates`] with its [`lex_core::candidates::CandidateDiagnostics`] from the same
/// run — so a price is always the price of the path the list shows.
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
/// rerank's argmin can differ between the populations and so can the top-1
/// (#361).
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
// Width disagreements and their exemptions
// ---------------------------------------------------------------------------

/// `#123` — the form every skip-like exemption must link (CLAUDE.md).
pub fn is_issue_ref(s: &str) -> bool {
    s.strip_prefix('#')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// One width whose top-1 differs from what the case expects.
#[derive(Debug, Clone, PartialEq)]
pub enum WidthMismatch {
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

/// The widths whose top-1 is not `expected` (the list's #1 is held to
/// `list_top`, which differs only for learned kana, `window_top1`).
pub fn width_disagreements(w: &Top1Widths, expected: &str, list_top: &str) -> Vec<WidthMismatch> {
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

/// A known width disagreement, named exactly: the issue tracking it and the
/// top-1 each disagreeing width shows. Only that disagreement is exempt —
/// any other width, a different top-1, or the no-history baseline still
/// fails the case, and so does the exemption once it no longer occurs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidthIssue {
    pub issue: String,
    /// What the synchronous 1-best shows instead of `expected`.
    #[serde(default)]
    pub one_best: Option<String>,
    /// What the production list's #1 is instead of `expected`.
    #[serde(default)]
    pub list_top: Option<String>,
}

/// A case's width disagreements, sorted by what the exemption says of them.
#[derive(Debug, Default, PartialEq)]
pub struct WidthVerdict {
    /// Not exempt: the case fails.
    pub unexempt: Vec<String>,
    /// Exempt and observed: reported only.
    pub known: Vec<String>,
    /// Exempt but not observed: the case fails until the exemption goes.
    pub stale: Vec<String>,
}

impl WidthIssue {
    /// Reject an exemption that is unlinked, names nothing, or names a
    /// value the width is supposed to show (it could never match).
    pub fn validate(&self, expected: &str, list_top: &str) -> Result<(), String> {
        if !is_issue_ref(&self.issue) {
            return Err(format!(
                "must link an issue like \"#123\", got {:?}",
                self.issue
            ));
        }
        if self.one_best.is_none() && self.list_top.is_none() {
            return Err("must name the disagreement it covers (one_best and/or list_top)".into());
        }
        if self.one_best.as_deref() == Some(expected) {
            return Err(format!(
                "one_best {expected:?} is the expected value, not a disagreement"
            ));
        }
        if self.list_top.as_deref() == Some(list_top) {
            return Err(format!(
                "list_top {list_top:?} is the expected value, not a disagreement"
            ));
        }
        Ok(())
    }

    fn covers(&self, m: &WidthMismatch) -> bool {
        match m {
            WidthMismatch::OneBest(got) => self.one_best.as_deref() == Some(got),
            WidthMismatch::ListTop { got, .. } => self.list_top.as_deref() == Some(got),
        }
    }
}

/// Sort `observed` by `issue` (no issue: every disagreement is unexempt).
pub fn judge_widths(issue: Option<&WidthIssue>, observed: &[WidthMismatch]) -> WidthVerdict {
    let mut v = WidthVerdict::default();
    for m in observed {
        if issue.is_some_and(|w| w.covers(m)) {
            v.known.push(m.to_string());
        } else {
            v.unexempt.push(m.to_string());
        }
    }
    if let Some(w) = issue {
        let seen = |one_best: bool| {
            observed
                .iter()
                .any(|m| matches!(m, WidthMismatch::OneBest(_)) == one_best && w.covers(m))
        };
        if let Some(val) = w.one_best.as_ref().filter(|_| !seen(true)) {
            v.stale.push(format!("1-best no longer shows {val}"));
        }
        if let Some(val) = w.list_top.as_ref().filter(|_| !seen(false)) {
            v.stale.push(format!("candidate #1 is no longer {val}"));
        }
    }
    v
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

/// Lines before 0-based index `from_line` are read but not parsed; indices
/// stay absolute. A window starting at the end of the log is empty (no line
/// written since); one starting past it is refused — a mistyped window must
/// not measure as "nothing to replay".
fn read_selections(path: &Path, from_line: usize) -> Result<Selections, String> {
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
            .map_err(|e| format!("read error at line index {i}: {e}"))?;
        if n == 0 {
            if from_line > i {
                return Err(format!(
                    "--from-line {from_line} is past the end of the log ({i} lines)"
                ));
            }
            break;
        }
        if i < from_line {
            continue;
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
    /// Selections priced below the #1 path. The #1 is then a learned
    /// surface the user picked around (history puts the cheapest learned
    /// path first, whatever cheaper unlearned paths follow it): a
    /// re-correction, the count PR-G's post-ship revert trigger reads.
    /// Learned is as of the `--history` given: count re-corrections against
    /// a copy of the history frozen at the window's start, since committing
    /// the surface learns it and a current history reads 0 here.
    pub gap_below_top: usize,
    /// Selections with no N-best path (lookup / prediction / injection only).
    pub gap_no_path: usize,
    /// Log lines skipped as unreadable (see `Selections::malformed`).
    pub malformed_lines: usize,
    /// First log line replayed (0-based; `--from-line`).
    pub from_line: usize,
    /// Distinct readings among the selections.
    pub readings: usize,
    /// First-page slots over every replayed selection's list: one window per
    /// selection, the population the user scanned.
    pub page1_slots: usize,
    /// Selections and first-page slots by the stage that put the surface on
    /// the list, one entry per owner (zeros included). Sums to `in_list`,
    /// `in_page` and `page1_slots`.
    pub by_owner: BTreeMap<Owner, OwnerCounts>,
}

/// Who placed a surface on the candidate list. Outside the N-best block it is
/// the stage whose insertion into the list's `seen` set succeeded. Inside it,
/// it is the stage that set the path's price (`priced_by`), not the one that
/// produced the path: the price decided the rank. A model path repriced by a
/// cheaper offer of the same surface is the offer's slot — the model did not
/// deliver that surface at that rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// Viterbi / Resegment: the cost model.
    Model,
    KanjiVariant,
    PartialHiragana,
    /// The kana rescue (#263, `HiraganaVariant`).
    Rescue,
    Numeric,
    Katakana,
    /// A learned surface no N-best path placed.
    Injected,
    /// The reading itself, added by the kana stage.
    Kana,
    /// Predictions and dictionary lookup.
    Tail,
}

impl Serialize for Owner {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl Owner {
    const ALL: [Self; 9] = [
        Self::Model,
        Self::KanjiVariant,
        Self::PartialHiragana,
        Self::Rescue,
        Self::Numeric,
        Self::Katakana,
        Self::Injected,
        Self::Kana,
        Self::Tail,
    ];

    fn priced_by(origin: PathOrigin) -> Self {
        match origin {
            // The origins `PathOrigin::is_model` names.
            PathOrigin::Viterbi | PathOrigin::Resegment => Self::Model,
            PathOrigin::KanjiVariant => Self::KanjiVariant,
            PathOrigin::PartialHiragana => Self::PartialHiragana,
            PathOrigin::HiraganaVariant => Self::Rescue,
            PathOrigin::Numeric => Self::Numeric,
            PathOrigin::Katakana => Self::Katakana,
        }
    }

    /// The name every report uses (text, `--verbose`, JSON keys).
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::KanjiVariant => "kanji_variant",
            Self::PartialHiragana => "partial_hiragana",
            Self::Rescue => "rescue",
            Self::Numeric => "numeric",
            Self::Katakana => "katakana",
            Self::Injected => "injected",
            Self::Kana => "kana",
            Self::Tail => "tail",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct OwnerCounts {
    /// Selections of this owner's surfaces (`in_page + off_page`).
    pub selections: usize,
    pub in_page: usize,
    pub off_page: usize,
    /// First-page slots this owner held.
    pub page1_slots: usize,
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
    /// The list's N-best paths as (surface, price) in list order — the
    /// cheapest learned path first, else the cheapest (so later paths can
    /// cost less than the first) — from the same run as `surfaces`. Joined
    /// surfaces are unique among paths.
    paths: Vec<(String, PathPrice)>,
    /// `owners[r]` put `surfaces[r]` on the list.
    owners: Vec<Owner>,
}

impl ReadingView {
    fn new(reading: &str, priced: PricedCandidates) -> Self {
        let PricedCandidates {
            response,
            diagnostics,
        } = priced;
        let paths: Vec<(String, PathPrice)> = response
            .paths
            .iter()
            .map(|p| joined_surface(p))
            .zip(diagnostics.prices)
            .collect();
        let owner = |surface: &str| {
            if let Some((_, price)) = paths.iter().find(|(s, _)| s == surface) {
                Owner::priced_by(price.priced_by)
            } else if diagnostics.injected.iter().any(|s| s == surface) {
                Owner::Injected
            } else if surface == reading {
                Owner::Kana
            } else {
                Owner::Tail
            }
        };
        let owners = response.surfaces.iter().map(|s| owner(s)).collect();
        Self {
            surfaces: response.surfaces,
            paths,
            owners,
        }
    }

    fn land(&self, surface: &str) -> Landing {
        let rank = self.surfaces.iter().position(|s| s == surface);
        let gap = self.paths.first().and_then(|(_, top)| {
            self.paths
                .iter()
                .find(|(s, _)| s == surface)
                .map(|(_, p)| p.cost - top.cost)
        });
        Landing { rank, gap }
    }
}

/// Where one selection landed in its reading's list.
struct Landing {
    rank: Option<usize>,
    /// Cost gap of the selected surface's N-best path to the #1 path;
    /// negative when #1 is a learned path priced above it.
    gap: Option<i64>,
}

/// The report's counts, accumulated one selection at a time.
struct Tally {
    rank_hist: Vec<usize>,
    gap_hist: Vec<usize>,
    gap_below_top: usize,
    gap_no_path: usize,
    by_owner: BTreeMap<Owner, OwnerCounts>,
}

impl Tally {
    fn new() -> Self {
        Self {
            rank_hist: vec![0; PAGE_SIZE],
            gap_hist: vec![0; GAP_BIN_UPPER.len() + 1],
            gap_below_top: 0,
            gap_no_path: 0,
            // Every owner has a row, so an owner with nothing reads as 0,
            // not as a missing key.
            by_owner: Owner::ALL.map(|o| (o, OwnerCounts::default())).into(),
        }
    }

    /// Count a selection of `surface` from `view`'s list: its rank and gap,
    /// its owner, and the first page the user scanned for it.
    fn add(&mut self, view: &ReadingView, surface: &str) -> Landing {
        let landing = view.land(surface);
        if let Some(r) = landing.rank {
            count_rank(&mut self.rank_hist, r);
            let c = self.by_owner.entry(view.owners[r]).or_default();
            c.selections += 1;
            if r < PAGE_SIZE {
                c.in_page += 1;
            } else {
                c.off_page += 1;
            }
        }
        match landing.gap {
            Some(g) if g < 0 => self.gap_below_top += 1,
            Some(g) => self.gap_hist[gap_bin(g)] += 1,
            None => self.gap_no_path += 1,
        }
        for &o in view.owners.iter().take(PAGE_SIZE) {
            self.by_owner.entry(o).or_default().page1_slots += 1;
        }
        landing
    }

    fn into_report(
        self,
        selections: usize,
        malformed_lines: usize,
        from_line: usize,
        readings: usize,
    ) -> ReplayReport {
        let in_list: usize = self.rank_hist.iter().sum();
        ReplayReport {
            selections,
            in_page: self.rank_hist.iter().take(PAGE_SIZE).sum(),
            in_list,
            absent: selections - in_list,
            rank_hist: self.rank_hist,
            gap_hist: self.gap_hist,
            gap_below_top: self.gap_below_top,
            gap_no_path: self.gap_no_path,
            malformed_lines,
            from_line,
            readings,
            page1_slots: self.by_owner.values().map(|c| c.page1_slots).sum(),
            by_owner: self.by_owner,
        }
    }
}

/// Replay every rank>0 selection in `log` from 0-based line `from_line`.
/// Returns the counts and one baseline line per selection (for
/// `--emit-baseline` / `--baseline`). A log with no rank>0 selection is a
/// valid, all-zero measurement. `verbose` prints per-selection content to
/// stderr (local inspection only).
pub fn replay(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    history: Option<&UserHistory>,
    log: &Path,
    from_line: usize,
    verbose: bool,
) -> Result<(ReplayReport, Vec<BaselineLine>), String> {
    let Selections {
        lines: selections,
        malformed,
    } = read_selections(log, from_line)?;

    let mut tally = Tally::new();
    let mut lines = Vec::with_capacity(selections.len());
    let mut cache: HashMap<&str, ReadingView> = HashMap::new();

    for (i, sel) in &selections {
        let reading = sel.reading.as_str();
        let view = cache.entry(reading).or_insert_with(|| {
            ReadingView::new(
                reading,
                production_candidates_priced(dict, conn, history, reading),
            )
        });
        let Landing { rank, gap } = tally.add(view, &sel.surface);
        if verbose {
            eprintln!(
                "{i}\t{}\t{}\trank={}\tgap={}\towner={}",
                sel.reading,
                sel.surface,
                rank.map_or("-".into(), |r| r.to_string()),
                gap.map_or("-".into(), |g| g.to_string()),
                rank.map_or("-", |r| view.owners[r].name()),
            );
        }
        lines.push(BaselineLine {
            i: *i,
            t: sel.t,
            rank,
        });
    }

    let report = tally.into_report(selections.len(), malformed, from_line, cache.len());
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
///
/// `from_line` is the replayed window's start: baseline lines before it are
/// outside the comparison, not "only in baseline".
pub fn diff_baseline(
    before: &[BaselineLine],
    after: &[BaselineLine],
    from_line: usize,
) -> BaselineDiff {
    let before_map: HashMap<(usize, u64), Option<usize>> = before
        .iter()
        .filter(|b| b.i >= from_line)
        .map(|b| ((b.i, b.t), b.rank))
        .collect();
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
    use lex_core::candidates::CandidateDiagnostics;

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
            diff_baseline(&before, &after, 0),
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
        let d = diff_baseline(&[line(0, 10, Some(1))], &[line(0, 20, None)], 0);
        assert_eq!(d.lost, 0);
        assert_eq!(d.only_current, 1);
        assert_eq!(d.only_baseline, 1);
    }

    /// A hand-built list: `nbest` are (surface, priced_by) paths in order,
    /// then `rest` in list order; `injected` names the injection stage's.
    fn view(
        reading: &str,
        nbest: &[(&str, PathOrigin)],
        rest: &[&str],
        injected: &[&str],
    ) -> ReadingView {
        let names: Vec<&str> = nbest.iter().map(|(s, _)| *s).collect();
        let mut response = resp(&names, &names);
        response.surfaces.extend(rest.iter().map(|s| s.to_string()));
        let diagnostics = CandidateDiagnostics {
            prices: nbest
                .iter()
                .enumerate()
                .map(|(i, &(_, priced_by))| PathPrice {
                    cost: 1000 * i as i64,
                    priced_by,
                })
                .collect(),
            injected: injected.iter().map(|s| s.to_string()).collect(),
        };
        ReadingView::new(
            reading,
            PricedCandidates {
                response,
                diagnostics,
            },
        )
    }

    #[test]
    fn owner_is_the_stage_that_listed_the_surface() {
        let v = view(
            "かな",
            &[
                ("仮名", PathOrigin::Viterbi),
                ("下名", PathOrigin::Resegment),
                ("か名", PathOrigin::PartialHiragana),
                ("化な", PathOrigin::KanjiVariant),
                ("カナ", PathOrigin::Katakana),
                ("かな2", PathOrigin::Numeric),
                ("かなー", PathOrigin::HiraganaVariant),
                ("可奈", PathOrigin::Viterbi),
            ],
            &["哉", "かな", "金"],
            // 可奈 is both learned and an N-best path: the path listed it.
            &["哉", "可奈"],
        );
        use Owner::*;
        assert_eq!(
            v.owners,
            [
                Model,
                Model,
                PartialHiragana,
                KanjiVariant,
                Katakana,
                Numeric,
                Rescue,
                Model,
                Injected,
                Kana,
                Tail
            ]
        );
    }

    #[test]
    fn owner_counts_sum_to_the_totals() {
        let long = view(
            "a",
            &[
                ("A0", PathOrigin::Viterbi),
                ("A1", PathOrigin::KanjiVariant),
            ],
            &["A2", "A3", "A4", "A5", "A6", "A7", "A8", "A9", "A10"],
            &["A9"],
        );
        let short = view("b", &[("B0", PathOrigin::Viterbi)], &["b"], &[]);
        let mut t = Tally::new();
        // The same window scanned three times counts three times.
        for _ in 0..3 {
            t.add(&long, "A1");
        }
        t.add(&long, "A9"); // off page, injected
        t.add(&long, "gone"); // absent
        t.add(&short, "b"); // a 2-entry list holds 2 slots
        let r = t.into_report(6, 0, 0, 2);

        assert_eq!(r.page1_slots, 5 * PAGE_SIZE + 2);
        let sum = |f: fn(&OwnerCounts) -> usize| r.by_owner.values().map(f).sum::<usize>();
        assert_eq!(sum(|c| c.selections), r.in_list);
        assert_eq!(sum(|c| c.in_page), r.in_page);
        assert_eq!(sum(|c| c.page1_slots), r.page1_slots);
        assert_eq!((r.in_list, r.absent), (5, 1));
        assert_eq!(
            r.by_owner[&Owner::KanjiVariant],
            OwnerCounts {
                selections: 3,
                in_page: 3,
                off_page: 0,
                page1_slots: 5,
            }
        );
        assert_eq!(
            r.by_owner[&Owner::Injected],
            OwnerCounts {
                selections: 1,
                in_page: 0,
                off_page: 1,
                page1_slots: 0,
            }
        );
        assert_eq!(r.by_owner[&Owner::Kana].page1_slots, 1);
    }

    /// A learned #1 priced above the path the user picks: the gap is
    /// negative and is counted apart from the bins (a re-correction).
    #[test]
    fn selection_below_a_learned_top_is_counted_apart() {
        let names = ["荷重", "二十", "二重"];
        let v = ReadingView::new(
            "にじゅう",
            PricedCandidates {
                response: resp(&names, &names),
                diagnostics: CandidateDiagnostics {
                    prices: [11456, 7334, 9000]
                        .into_iter()
                        .map(|cost| PathPrice {
                            cost,
                            priced_by: PathOrigin::Viterbi,
                        })
                        .collect(),
                    injected: Vec::new(),
                },
            },
        );
        let mut t = Tally::new();
        t.add(&v, "二十");
        t.add(&v, "荷重");
        let r = t.into_report(2, 0, 0, 1);
        assert_eq!(r.gap_below_top, 1);
        assert_eq!(r.gap_hist[0], 1, "the #1 itself is at gap 0");
        assert_eq!(r.gap_hist.iter().sum::<usize>(), 1);
    }

    #[test]
    fn report_json_names_owners_in_snake_case() {
        let mut t = Tally::new();
        t.add(
            &view("a", &[("A", PathOrigin::PartialHiragana)], &[], &[]),
            "A",
        );
        let json = serde_json::to_value(t.into_report(1, 0, 7, 1)).unwrap();
        assert_eq!(json["from_line"], 7);
        assert_eq!(
            json["by_owner"]["kanji_variant"]["selections"], 0,
            "zeros are listed"
        );
        assert_eq!(
            json["by_owner"].as_object().unwrap().len(),
            Owner::ALL.len()
        );
        assert_eq!(json["by_owner"]["partial_hiragana"]["selections"], 1);
        assert_eq!(json["by_owner"]["partial_hiragana"]["page1_slots"], 1);
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

        let sels = read_selections(&path, 0).unwrap();
        assert_eq!(sels.malformed, 2);
        assert_eq!(sels.lines.len(), 1, "only rank>0 lines are replayed");
        assert_eq!((sels.lines[0].0, sels.lines[0].1.t), (1, 2));

        // No rank>0 line at all is an empty measurement, not an error.
        fs::write(
            &path,
            "{\"t\":1,\"reading\":\"あ\",\"surface\":\"亜\",\"rank\":0}\n",
        )
        .unwrap();
        let sels = read_selections(&path, 0).unwrap();
        assert!(sels.lines.is_empty());
        assert_eq!(sels.malformed, 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn from_line_skips_earlier_lines_and_keeps_absolute_indices() {
        let dir = std::env::temp_dir().join(format!("lexcli-from-line-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("commit-log.jsonl");
        let sel =
            |t: u64| format!("{{\"t\":{t},\"reading\":\"い\",\"surface\":\"胃\",\"rank\":1}}\n");
        fs::write(&path, format!("{}not json\n{}{}", sel(1), sel(3), sel(4))).unwrap();

        let sels = read_selections(&path, 2).unwrap();
        let got: Vec<(usize, u64)> = sels.lines.iter().map(|(i, l)| (*i, l.t)).collect();
        assert_eq!(got, [(2, 3), (3, 4)]);
        assert_eq!(sels.malformed, 0, "skipped lines are not read");

        let sels = read_selections(&path, 4).unwrap();
        assert!(sels.lines.is_empty(), "a window at EOF is empty");
        assert!(read_selections(&path, 5).is_err(), "past EOF is refused");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn baseline_diff_ignores_lines_before_the_window() {
        let before = [line(0, 10, Some(1)), line(5, 15, Some(2))];
        let after = [line(5, 15, Some(2))];
        let d = diff_baseline(&before, &after, 5);
        assert_eq!((d.unchanged, d.only_baseline, d.only_current), (1, 0, 0));
    }

    fn issue(one_best: Option<&str>, list_top: Option<&str>) -> WidthIssue {
        WidthIssue {
            issue: "#1".into(),
            one_best: one_best.map(Into::into),
            list_top: list_top.map(Into::into),
        }
    }

    fn list_top(got: &str) -> WidthMismatch {
        WidthMismatch::ListTop {
            got: got.into(),
            want: "期".into(),
        }
    }

    #[test]
    fn width_exemption_covers_only_the_named_disagreement() {
        let w = issue(None, Some("二個"));
        let observed = [list_top("二個"), WidthMismatch::OneBest("他".into())];
        let v = judge_widths(Some(&w), &observed);
        assert_eq!(v.known.len(), 1);
        assert_eq!(v.unexempt, ["1-best shows 他"], "another width still fails");
        assert!(v.stale.is_empty());

        // A different top-1 at the named width is not the known one.
        let v = judge_widths(Some(&w), &[list_top("三個")]);
        assert_eq!(v.unexempt.len(), 1);
        assert_eq!(v.stale, ["candidate #1 is no longer 二個"]);

        // Without an exemption everything is unexempt.
        let v = judge_widths(None, &observed);
        assert_eq!((v.unexempt.len(), v.known.len()), (2, 0));
    }

    #[test]
    fn width_exemption_that_no_longer_occurs_is_stale() {
        let v = judge_widths(
            Some(&issue(Some("に個"), Some("二個"))),
            &[list_top("二個")],
        );
        assert_eq!(v.stale, ["1-best no longer shows に個"]);
        let v = judge_widths(Some(&issue(None, Some("二個"))), &[]);
        assert_eq!(v.stale, ["candidate #1 is no longer 二個"]);
    }

    #[test]
    fn width_exemption_validation() {
        assert!(issue(None, Some("二個")).validate("期", "期").is_ok());
        assert!(issue(None, None).validate("期", "期").is_err());
        assert!(
            issue(Some("期"), None).validate("期", "期").is_err(),
            "names the expected value"
        );
        assert!(issue(None, Some("期")).validate("期", "期").is_err());
        let mut unlinked = issue(None, Some("二個"));
        unlinked.issue = "later".into();
        assert!(unlinked.validate("期", "期").is_err());
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
