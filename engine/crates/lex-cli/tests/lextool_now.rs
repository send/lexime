//! T-N2: `lextool --now <epoch>` pins every history decay a subcommand does.
//!
//! Each history-reading subcommand runs as a separate process, twice at the
//! same `--now` with a clock second between them: the outputs (stdout,
//! stderr, the files written) must match byte for byte. A third run at a
//! `--now` a long way on must differ, or the flag is not wired to that
//! subcommand and the run was measuring the wall clock all along.
//!
//! The history was last used at `T0`, ahead of any wall clock this runs
//! under (a negative age counts as 0). `HALF` is one half-life after it:
//! the learned 京 holds half its boost. `STALE` is ~1100 years on: it holds
//! none. A subcommand reading the wall clock would see the boost whole, and
//! match neither.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::{DictEntry, TrieDictionary};
use lex_core::user_history::UserHistory;

const T0: u64 = 4_000_000_000;
const HALF: u64 = T0 + 168 * 3600;
const STALE: u64 = T0 + 10_000_000 * 3600;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("lextool-now-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = Fixture { dir };

        let e = |surface: &str, cost: i16| DictEntry {
            surface: surface.into(),
            cost,
            left_id: 0,
            right_id: 0,
        };
        TrieDictionary::from_entries(vec![
            ("きょう".to_string(), vec![e("今日", 3000), e("京", 5000)]),
            ("きょうと".to_string(), vec![e("京都", 4000)]),
            ("は".to_string(), vec![e("は", 2000)]),
        ])
        .save(&f.path("lexime.dict"))
        .unwrap();
        // One id: every connection costs 0.
        ConnectionMatrix::from_text("1\n0\n")
            .unwrap()
            .save(&f.path("lexime.conn"))
            .unwrap();

        // 京 twice (audited needs frequency 2) and 今日→は once, all at T0.
        let mut h = UserHistory::new();
        for _ in 0..2 {
            h.record_at(&[("きょう".into(), "京".into())], T0);
        }
        h.record_at(
            &[("きょう".into(), "今日".into()), ("は".into(), "は".into())],
            T0,
        );
        h.save(&f.path("history.lxud")).unwrap();

        fs::write(f.path("readings.txt"), "きょう\nきょうと\n").unwrap();
        fs::write(
            f.path("corpus.toml"),
            "[[cases]]\nreading = \"きょう\"\nexpected = \"京\"\ncategory = \"history\"\n",
        )
        .unwrap();
        // The same expectation, but the history is seeded by the run itself.
        fs::write(
            f.path("corpus-seeded.toml"),
            "[[history]]\nsegments = [[\"きょう\", \"京\"]]\nrepeat = 2\n\n\
             [[cases]]\nreading = \"きょう\"\nexpected = \"京\"\ncategory = \"history\"\n",
        )
        .unwrap();
        fs::write(
            f.path("commit-log.jsonl"),
            "{\"t\":1,\"reading\":\"きょう\",\"surface\":\"京\",\"rank\":1,\"top1\":\"今日\"}\n\
             {\"t\":2,\"reading\":\"きょう\",\"surface\":\"今日\",\"rank\":0}\n",
        )
        .unwrap();
        // The baseline diff-snapshot compares against, written at HALF
        // whatever `now` that subcommand later runs at.
        f.lextool(
            &[
                "snapshot",
                &f.arg("lexime.dict"),
                &f.arg("lexime.conn"),
                &f.arg("readings.txt"),
                &f.arg("base.jsonl"),
                "--history",
                &f.arg("history.lxud"),
            ],
            &[],
            HALF,
        );
        f
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn arg(&self, name: &str) -> String {
        self.path(name).to_str().unwrap().to_string()
    }

    /// One `lextool` process; its observable output as bytes.
    fn lextool(&self, args: &[&str], outputs: &[&str], now: u64) -> Vec<u8> {
        let out = Command::new(env!("CARGO_BIN_EXE_lextool"))
            .args(args)
            .args(["--now", &now.to_string()])
            .output()
            .unwrap();
        let mut seen = Vec::new();
        seen.extend(format!("status={:?}\n--stdout\n", out.status.code()).bytes());
        seen.extend(&out.stdout);
        seen.extend(b"\n--stderr\n");
        seen.extend(&out.stderr);
        for name in outputs {
            seen.extend(format!("\n--file {name}\n").bytes());
            seen.extend(fs::read(self.path(name)).unwrap_or_default());
        }
        seen
    }

    /// Every history-reading subcommand at `now`: (name, output).
    fn run_all(&self, now: u64) -> Vec<(&'static str, Vec<u8>)> {
        let dict = self.arg("lexime.dict");
        let conn = self.arg("lexime.conn");
        let hist = self.arg("history.lxud");
        let readings = self.arg("readings.txt");
        let snap = self.arg("snap.jsonl");
        let mut runs = Vec::new();
        let mut run = |name, args: &[&str], outputs: &[&str]| {
            runs.push((name, self.lextool(args, outputs, now)));
        };
        for (name, kind) in [
            ("snapshot", None),
            ("snapshot --candidates", Some("--candidates")),
            ("snapshot --predictive", Some("--predictive")),
        ] {
            let mut args = vec![
                "snapshot",
                &dict,
                &conn,
                &readings,
                &snap,
                "--history",
                &hist,
            ];
            args.extend(kind);
            run(name, &args, &["snap.jsonl"]);
        }
        run(
            "diff-snapshot",
            &[
                "diff-snapshot",
                &dict,
                &conn,
                &readings,
                &self.arg("base.jsonl"),
                "--history",
                &hist,
            ],
            &[],
        );
        run(
            "explain",
            &[
                "explain",
                &dict,
                "きょう",
                "--conn",
                &conn,
                "--history",
                &hist,
            ],
            &[],
        );
        run(
            "explain --json",
            &[
                "explain",
                &dict,
                "きょう",
                "--conn",
                &conn,
                "--history",
                &hist,
                "--json",
            ],
            &[],
        );
        run(
            "accuracy --history",
            &[
                "accuracy",
                &dict,
                &conn,
                &self.arg("corpus.toml"),
                "--verbose",
                "--history",
                &hist,
            ],
            &[],
        );
        run(
            "accuracy [[history]]",
            &[
                "accuracy",
                &dict,
                &conn,
                &self.arg("corpus-seeded.toml"),
                "--verbose",
            ],
            &[],
        );
        run(
            "replay-commit-log",
            &[
                "replay-commit-log",
                &dict,
                &conn,
                &self.arg("commit-log.jsonl"),
                "--history",
                &hist,
                "--json",
            ],
            &[],
        );
        run(
            "history-audit",
            &["history-audit", &dict, &conn, &hist, "--json"],
            &[],
        );
        runs
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn now_pins_every_history_reading_subcommand() {
    let fx = Fixture::new();
    let first = fx.run_all(HALF);
    // Each run did its job: no usage error or unreadable input stands in for
    // an output that merely repeats.
    for (name, out) in &first {
        let text = String::from_utf8_lossy(out);
        assert!(
            text.starts_with("status=Some(0)") && !text.contains("error"),
            "{name}: {text}"
        );
    }
    sleep(Duration::from_millis(1100));
    let second = fx.run_all(HALF);
    let stale = fx.run_all(STALE);

    for (((name, a), (_, b)), (_, c)) in first.iter().zip(&second).zip(&stale) {
        let text = |v: &Vec<u8>| String::from_utf8_lossy(v).into_owned();
        assert_eq!(
            a,
            b,
            "{name}: two runs at the same --now differ\n{}\n{}",
            text(a),
            text(b)
        );
        if *name == "accuracy [[history]]" {
            // Seeded by the run at its own `now`, so every `now` agrees. A
            // seeding at the wall clock instead would be decayed to nothing
            // by a `--now` this far on.
            assert_eq!(
                a,
                c,
                "{name}: seeding does not follow --now\n{}\n{}",
                text(a),
                text(c)
            );
        } else {
            assert_ne!(
                a,
                c,
                "{name}: --now does not reach this subcommand\n{}",
                text(a)
            );
        }
    }
}
