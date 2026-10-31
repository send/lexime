//! Speed / 1-best probe for A/B comparisons of the conversion hot path
//! (driven by `scripts/bench/probe-bench.py`; built by
//! `scripts/bench/build-probe.sh`).
//!
//! Timing: the inputs are 10 fixed readings plus `count - 10` lines of the
//! readings file picked by a seeded shuffle; each mode prints
//! `<mode> total_mean_ns <ns>`, the sum over the inputs of the per-input
//! mean. `--oneshot` instead writes "reading<TAB>1-best" for every line of
//! the readings file and exits (no timing). hist1 / nohist1 time
//! `convert_from_lattice` on a lattice built outside the timed loop (the
//! session reuses its lattice across keystrokes); cands times
//! `generate_candidates` whole, lattice included.
//!
//! Pass the same `--now` to every arm: history decay is read at that time.
//! The history is opened (and may be rewritten), so give each run its own
//! copy.
use clap::{Parser, ValueEnum};
use lex_core::converter::{ConversionContext, ConvertedSegment};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use lex_core::user_dict::UserDictionary;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::time::Instant;

const A5: [&str; 5] = [
    "きょう",
    "きょうはてんきがいい",
    "きょうはてんきがいいのでさんぽにでかけました",
    "とうきょうとしぶやくにあるかいしゃではたらいています",
    "きのうのかいぎであたらしいぷろじぇくとのけいかくについてはなしあいました",
];
const B5: [&str; 5] = [
    "かくにんしますね",
    "かくにんして",
    "したんでしたっけ",
    "せつめいして",
    "いいきがしてきました",
];

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum DictKind {
    Trie,
    Composite,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Mode {
    Hist1,
    Nohist1,
    Cands,
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dict_file: PathBuf,
    #[arg(long)]
    conn_file: PathBuf,
    #[arg(long)]
    readings_file: PathBuf,
    /// Layered over the system dictionary with `--dict composite`
    #[arg(long)]
    user_dict: PathBuf,
    /// The checkpoint (.lxud); its WAL is replayed
    #[arg(long)]
    history: PathBuf,
    /// A settings.toml to run under instead of the embedded settings
    #[arg(long)]
    settings: Option<PathBuf>,
    /// Unix time (seconds) history decay is read at; default: the wall clock
    #[arg(long)]
    now: Option<u64>,
    #[arg(long, value_enum, default_value = "composite")]
    dict: DictKind,
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "hist1,nohist1,cands"
    )]
    modes: Vec<Mode>,
    #[arg(long, default_value_t = 20261018)]
    seed: u64,
    #[arg(long, default_value_t = 200)]
    count: usize,
    #[arg(long, default_value_t = 20)]
    warmup: usize,
    #[arg(long, default_value_t = 200)]
    iters: usize,
    /// Write the hist1 1-best of each input here
    #[arg(long)]
    surfaces_out: Option<PathBuf>,
    /// Write every reading's 1-best here and exit (no timing)
    #[arg(long)]
    oneshot: Option<PathBuf>,
    /// With --oneshot: convert without the history
    #[arg(long, requires = "oneshot")]
    nohist: bool,
}

fn joined(segs: &[ConvertedSegment]) -> String {
    segs.iter().map(|s| s.surface.as_str()).collect()
}

fn die(msg: String) -> ! {
    eprintln!("speed_probe: {msg}");
    process::exit(1);
}

/// As lextool's `open_history`: `open_with_wal` returns an empty history for
/// a missing checkpoint and WAL, and a mistyped path must not measure an
/// unlearned engine as the learned one.
fn open_history(checkpoint: &Path, now: u64) -> lex_core::user_history::UserHistory {
    let wal = checkpoint.with_extension("lxud.wal");
    if !checkpoint.exists() && !wal.exists() {
        die(format!(
            "user history not found: neither {} nor {} exists",
            checkpoint.display(),
            wal.display()
        ));
    }
    lex_core::user_history::wal::open_with_wal(checkpoint, now)
        .unwrap_or_else(|e| die(format!("open {}: {e}", checkpoint.display())))
        .0
}

fn main() {
    let a = Args::parse();
    if let Some(p) = &a.settings {
        let text = std::fs::read_to_string(p)
            .unwrap_or_else(|e| die(format!("read {}: {e}", p.display())));
        lex_core::settings::init_custom(text).unwrap_or_else(|e| die(format!("settings: {e}")));
    }
    let st = lex_core::settings::settings();
    // The arm's settings are shown by the probe itself.
    eprintln!(
        "settings: nbest={} max_results={} max_cost_gap={} boost_per_use={} max_boost={} half_life_hours={} max_unigrams={}",
        st.candidates.nbest,
        st.candidates.max_results,
        st.candidates.max_cost_gap,
        st.history.boost_per_use,
        st.history.max_boost,
        st.history.half_life_hours,
        st.history.max_unigrams
    );
    let now = a.now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });
    eprintln!("now={now}");
    let trie = TrieDictionary::open(&a.dict_file).unwrap_or_else(|e| die(format!("dict: {e}")));
    let conn = ConnectionMatrix::open(&a.conn_file).unwrap_or_else(|e| die(format!("conn: {e}")));
    let dict: Box<dyn Dictionary> = match a.dict {
        DictKind::Trie => Box::new(trie),
        DictKind::Composite => {
            let ud = UserDictionary::open(&a.user_dict)
                .unwrap_or_else(|e| die(format!("user dict: {e}")));
            Box::new(CompositeDictionary::new(vec![Arc::new(trie), Arc::new(ud)]))
        }
    };
    let hist = open_history(&a.history, now);
    let mk_ctx = |with_hist: bool| {
        ConversionContext::new(&*dict, Some(&conn)).with_history(with_hist.then_some(&hist), now)
    };
    let readings = std::fs::read_to_string(&a.readings_file)
        .unwrap_or_else(|e| die(format!("read {}: {e}", a.readings_file.display())));

    if let Some(of) = &a.oneshot {
        let ctx = mk_ctx(!a.nohist);
        let mut o = String::new();
        for r in readings.lines().filter(|s| !s.is_empty()) {
            let lattice = ctx.build_lattice(r);
            o.push_str(&format!(
                "{}\t{}\n",
                r,
                joined(&ctx.convert_from_lattice(&lattice))
            ));
        }
        std::fs::write(of, o).unwrap_or_else(|e| die(format!("write {}: {e}", of.display())));
        return;
    }

    // Inputs: A5 + B5 + (count - 10) readings picked by a seeded partial
    // Fisher-Yates (xorshift64*).
    let mut l: Vec<&str> = readings.lines().filter(|s| !s.is_empty()).collect();
    let mut s = a.seed | 1;
    let mut rnd = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545F4914F6CDD1D)
    };
    let want = a.count.saturating_sub(10).min(l.len());
    for i in 0..want {
        let j = i + (rnd() as usize) % (l.len() - i);
        l.swap(i, j);
    }
    let inputs: Vec<&str> = A5
        .iter()
        .chain(B5.iter())
        .copied()
        .chain(l.into_iter().take(want))
        .collect();
    eprintln!(
        "inputs={} seed={} warmup={} iters={}",
        inputs.len(),
        a.seed,
        a.warmup,
        a.iters
    );
    let max = st.candidates.max_results;
    let ctx_h = mk_ctx(true);
    let ctx_n = mk_ctx(false);
    let mut surf = String::new();
    for &mode in &a.modes {
        let ctx = if mode == Mode::Nohist1 {
            &ctx_n
        } else {
            &ctx_h
        };
        let mut total_ns: f64 = 0.0;
        for r in &inputs {
            let lattice = ctx.build_lattice(r);
            let one = || match mode {
                Mode::Hist1 | Mode::Nohist1 => {
                    black_box(ctx.convert_from_lattice(black_box(&lattice)));
                }
                Mode::Cands => {
                    black_box(
                        lex_core::candidates::generate_candidates(ctx, black_box(r), max).surfaces,
                    );
                }
            };
            for _ in 0..a.warmup {
                one();
            }
            let t = Instant::now();
            for _ in 0..a.iters {
                one();
            }
            total_ns += t.elapsed().as_nanos() as f64 / a.iters as f64;
            if mode == Mode::Hist1 && a.surfaces_out.is_some() {
                surf.push_str(&joined(&ctx.convert_from_lattice(&lattice)));
                surf.push('\n');
            }
        }
        let name = mode.to_possible_value().expect("no skipped variants");
        println!("{} total_mean_ns {total_ns:.0}", name.get_name());
    }
    if let Some(f) = &a.surfaces_out {
        std::fs::write(f, surf).unwrap_or_else(|e| die(format!("write {}: {e}", f.display())));
    }
}
