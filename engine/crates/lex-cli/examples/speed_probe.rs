//! Speed / 1-best probe for A/B comparisons of the conversion hot path
//! (driven by `scripts/bench/probe-bench.py`; built by
//! `scripts/bench/build-probe.sh`).
//!
//! ```text
//! speed_probe --dict-file D --conn-file C --readings-file R --user-dict U --history H
//!     [--settings T] [--now EPOCH] [--dict trie|composite] [--modes hist1,nohist1,cands]
//!     [--seed N] [--count 200] [--warmup 20] [--iters 200] [--surfaces-out F]
//!     [--expect-max-cost-gap N] [--expect-nbest N]
//! speed_probe ... --oneshot OUT [--nohist]
//! ```
//!
//! Timing: the inputs are 10 fixed readings plus `count - 10` lines of R
//! picked by a seeded shuffle; each mode prints `<mode> total_mean_ns <ns>`,
//! the sum over the inputs of the per-input mean. `--oneshot` instead writes
//! "reading<TAB>1-best" for every line of R and exits (no timing).
//!
//! Pass the same `--now` to every arm: history decay is read at that time.
//! H is opened (and may be rewritten), so give each run its own copy.
use lex_core::converter::{ConversionContext, ConvertedSegment};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use lex_core::user_dict::UserDictionary;
use std::hint::black_box;
use std::path::Path;
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

fn arg(a: &[String], k: &str) -> Option<String> {
    a.iter()
        .position(|x| x == k)
        .and_then(|i| a.get(i + 1).cloned())
}

fn joined(segs: &[ConvertedSegment]) -> String {
    segs.iter().map(|s| s.surface.as_str()).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let req = |k: &str| arg(&a, k).unwrap_or_else(|| panic!("missing {k}"));
    if let Some(p) = arg(&a, "--settings") {
        lex_core::settings::init_custom(std::fs::read_to_string(&p).expect("settings"))
            .expect("init_custom");
    }
    let st = lex_core::settings::settings();
    // The arm's settings are shown (and checked) by the probe itself.
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
    if let Some(e) = arg(&a, "--expect-max-cost-gap") {
        assert_eq!(
            st.candidates.max_cost_gap,
            e.parse::<i64>().unwrap(),
            "max_cost_gap"
        );
    }
    if let Some(e) = arg(&a, "--expect-nbest") {
        assert_eq!(st.candidates.nbest, e.parse::<usize>().unwrap(), "nbest");
    }
    let now: u64 = arg(&a, "--now").map_or_else(
        || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        },
        |n| n.parse().unwrap(),
    );
    eprintln!("now={now}");
    let trie = TrieDictionary::open(Path::new(&req("--dict-file"))).expect("dict");
    let conn = ConnectionMatrix::open(Path::new(&req("--conn-file"))).expect("conn");
    let kind = arg(&a, "--dict").unwrap_or_else(|| "composite".into());
    let dict: Box<dyn Dictionary> = match kind.as_str() {
        "trie" => Box::new(trie),
        "composite" => {
            let ud = UserDictionary::open(Path::new(&req("--user-dict"))).expect("user dict");
            Box::new(CompositeDictionary::new(vec![Arc::new(trie), Arc::new(ud)]))
        }
        o => panic!("--dict {o}"),
    };
    let hist = lex_core::user_history::wal::open_with_wal(Path::new(&req("--history")), now)
        .expect("open history")
        .0;
    let mk_ctx = |with_hist: bool| {
        ConversionContext::new(&*dict, Some(&conn)).with_history(with_hist.then_some(&hist), now)
    };
    let readings = std::fs::read_to_string(req("--readings-file")).expect("readings");

    if let Some(of) = arg(&a, "--oneshot") {
        let ctx = mk_ctx(!a.iter().any(|x| x == "--nohist"));
        let mut o = String::new();
        for r in readings.lines().filter(|s| !s.is_empty()) {
            let lattice = ctx.build_lattice(r);
            o.push_str(&format!(
                "{}\t{}\n",
                r,
                joined(&ctx.convert_from_lattice(&lattice))
            ));
        }
        std::fs::write(of, o).unwrap();
        return;
    }

    let seed: u64 = arg(&a, "--seed").map_or(20261018, |s| s.parse().unwrap());
    let count: usize = arg(&a, "--count").map_or(200, |s| s.parse().unwrap());
    let warmup: usize = arg(&a, "--warmup").map_or(20, |s| s.parse().unwrap());
    let iters: usize = arg(&a, "--iters").map_or(200, |s| s.parse().unwrap());
    // Inputs: A5 + B5 + (count - 10) readings picked by a seeded partial
    // Fisher-Yates (xorshift64*).
    let mut l: Vec<&str> = readings.lines().filter(|s| !s.is_empty()).collect();
    let mut s = seed | 1;
    let mut rnd = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545F4914F6CDD1D)
    };
    let want = count.saturating_sub(10).min(l.len());
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
        "inputs={} seed={seed} warmup={warmup} iters={iters} dict={kind}",
        inputs.len()
    );
    let modes = arg(&a, "--modes").unwrap_or_else(|| "hist1,nohist1,cands".into());
    let max = st.candidates.max_results;
    let ctx_h = mk_ctx(true);
    let ctx_n = mk_ctx(false);
    let mut surf = String::new();
    for mode in modes.split(',') {
        let ctx = if mode == "nohist1" { &ctx_n } else { &ctx_h };
        let mut total_ns: f64 = 0.0;
        for r in &inputs {
            let lattice = ctx.build_lattice(r);
            let one = || match mode {
                "hist1" | "nohist1" => {
                    black_box(ctx.convert_from_lattice(black_box(&lattice)));
                }
                "cands" => {
                    black_box(
                        lex_core::candidates::generate_candidates(ctx, black_box(r), max).surfaces,
                    );
                }
                o => panic!("mode {o}"),
            };
            for _ in 0..warmup {
                one();
            }
            let t = Instant::now();
            for _ in 0..iters {
                one();
            }
            total_ns += t.elapsed().as_nanos() as f64 / iters as f64;
            if mode == "hist1" {
                surf.push_str(&joined(&ctx.convert_from_lattice(&lattice)));
                surf.push('\n');
            }
        }
        println!("{mode} total_mean_ns {total_ns:.0}");
    }
    if let Some(f) = arg(&a, "--surfaces-out") {
        std::fs::write(f, surf).unwrap();
    }
}
