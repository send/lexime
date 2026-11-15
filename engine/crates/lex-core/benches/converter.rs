use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use lex_core::converter::{build_lattice, convert, convert_nbest};
use lex_core::dict::{DictEntry, TrieDictionary};
use lex_core::user_history::UserHistory;

fn bench_dict() -> TrieDictionary {
    let entries = vec![
        (
            "きょう".into(),
            vec![
                DictEntry {
                    surface: "今日".into(),
                    cost: 3000,
                    left_id: 100,
                    right_id: 100,
                    symbol: false,
                },
                DictEntry {
                    surface: "京".into(),
                    cost: 5000,
                    left_id: 101,
                    right_id: 101,
                    symbol: false,
                },
            ],
        ),
        (
            "は".into(),
            vec![DictEntry {
                surface: "は".into(),
                cost: 2000,
                left_id: 200,
                right_id: 200,
                symbol: false,
            }],
        ),
        (
            "いい".into(),
            vec![
                DictEntry {
                    surface: "良い".into(),
                    cost: 3500,
                    left_id: 300,
                    right_id: 300,
                    symbol: false,
                },
                DictEntry {
                    surface: "いい".into(),
                    cost: 4000,
                    left_id: 301,
                    right_id: 301,
                    symbol: false,
                },
            ],
        ),
        (
            "てんき".into(),
            vec![DictEntry {
                surface: "天気".into(),
                cost: 4000,
                left_id: 400,
                right_id: 400,
                symbol: false,
            }],
        ),
        (
            "です".into(),
            vec![DictEntry {
                surface: "です".into(),
                cost: 2500,
                left_id: 800,
                right_id: 800,
                symbol: false,
            }],
        ),
        (
            "ね".into(),
            vec![DictEntry {
                surface: "ね".into(),
                cost: 2000,
                left_id: 900,
                right_id: 900,
                symbol: false,
            }],
        ),
        (
            "わたし".into(),
            vec![DictEntry {
                surface: "私".into(),
                cost: 3000,
                left_id: 1000,
                right_id: 1000,
                symbol: false,
            }],
        ),
        (
            "だ".into(),
            vec![DictEntry {
                surface: "だ".into(),
                cost: 2500,
                left_id: 810,
                right_id: 810,
                symbol: false,
            }],
        ),
        (
            "と".into(),
            vec![DictEntry {
                surface: "と".into(),
                cost: 2000,
                left_id: 820,
                right_id: 820,
                symbol: false,
            }],
        ),
        (
            "おもい".into(),
            vec![DictEntry {
                surface: "思い".into(),
                cost: 3500,
                left_id: 830,
                right_id: 830,
                symbol: false,
            }],
        ),
        (
            "おもいます".into(),
            vec![DictEntry {
                surface: "思います".into(),
                cost: 3200,
                left_id: 831,
                right_id: 831,
                symbol: false,
            }],
        ),
        (
            "ます".into(),
            vec![DictEntry {
                surface: "ます".into(),
                cost: 2500,
                left_id: 840,
                right_id: 840,
                symbol: false,
            }],
        ),
        (
            "い".into(),
            vec![DictEntry {
                surface: "胃".into(),
                cost: 6000,
                left_id: 600,
                right_id: 600,
                symbol: false,
            }],
        ),
        (
            "き".into(),
            vec![DictEntry {
                surface: "木".into(),
                cost: 4500,
                left_id: 500,
                right_id: 500,
                symbol: false,
            }],
        ),
        (
            "てん".into(),
            vec![DictEntry {
                surface: "天".into(),
                cost: 5000,
                left_id: 700,
                right_id: 700,
                symbol: false,
            }],
        ),
        (
            "がくせい".into(),
            vec![DictEntry {
                surface: "学生".into(),
                cost: 4000,
                left_id: 1100,
                right_id: 1100,
                symbol: false,
            }],
        ),
        (
            "しゅくだい".into(),
            vec![DictEntry {
                surface: "宿題".into(),
                cost: 4000,
                left_id: 1200,
                right_id: 1200,
                symbol: false,
            }],
        ),
        (
            "を".into(),
            vec![DictEntry {
                surface: "を".into(),
                cost: 2000,
                left_id: 210,
                right_id: 210,
                symbol: false,
            }],
        ),
        (
            "やる".into(),
            vec![DictEntry {
                surface: "やる".into(),
                cost: 3500,
                left_id: 850,
                right_id: 850,
                symbol: false,
            }],
        ),
        (
            "の".into(),
            vec![DictEntry {
                surface: "の".into(),
                cost: 2000,
                left_id: 220,
                right_id: 220,
                symbol: false,
            }],
        ),
        (
            "が".into(),
            vec![DictEntry {
                surface: "が".into(),
                cost: 2000,
                left_id: 230,
                right_id: 230,
                symbol: false,
            }],
        ),
        (
            "めんどう".into(),
            vec![DictEntry {
                surface: "面倒".into(),
                cost: 4500,
                left_id: 860,
                right_id: 860,
                symbol: false,
            }],
        ),
        (
            "くさい".into(),
            vec![DictEntry {
                surface: "臭い".into(),
                cost: 5000,
                left_id: 870,
                right_id: 870,
                symbol: false,
            }],
        ),
        (
            "めんどうくさい".into(),
            vec![DictEntry {
                surface: "面倒くさい".into(),
                cost: 3800,
                left_id: 861,
                right_id: 861,
                symbol: false,
            }],
        ),
        (
            "けど".into(),
            vec![DictEntry {
                surface: "けど".into(),
                cost: 2500,
                left_id: 880,
                right_id: 880,
                symbol: false,
            }],
        ),
        (
            "がんばり".into(),
            vec![DictEntry {
                surface: "頑張り".into(),
                cost: 4000,
                left_id: 890,
                right_id: 890,
                symbol: false,
            }],
        ),
        (
            "がんばります".into(),
            vec![DictEntry {
                surface: "頑張ります".into(),
                cost: 3500,
                left_id: 891,
                right_id: 891,
                symbol: false,
            }],
        ),
    ];
    TrieDictionary::from_entries(entries)
}

static INPUTS: &[(&str, &str)] = &[
    ("short", "きょう"),
    ("medium", "きょうはいいてんきですね"),
    ("long", "わたしはきょうはいいてんきだとおもいます"),
    (
        "very_long",
        "わたしはきょうしゅくだいをやるのがめんどうくさいけどがんばります",
    ),
];

fn bench_build_lattice(c: &mut Criterion) {
    let dict = bench_dict();
    let mut group = c.benchmark_group("converter/build_lattice");
    for &(label, kana) in INPUTS {
        group.bench_with_input(BenchmarkId::new(label, kana.len()), &kana, |b, &kana| {
            b.iter(|| build_lattice(&dict, kana));
        });
    }
    group.finish();
}

fn bench_convert_1best(c: &mut Criterion) {
    let dict = bench_dict();
    let mut group = c.benchmark_group("converter/convert_1best");
    for &(label, kana) in INPUTS {
        group.bench_with_input(BenchmarkId::new(label, kana.len()), &kana, |b, &kana| {
            b.iter(|| convert(&dict, None, kana));
        });
    }
    group.finish();
}

/// A small non-empty history: the IME always attaches one, and with any
/// learning the 1-best runs the offer rewriters (history can lift an offer
/// to index 0).
fn bench_history() -> UserHistory {
    let mut h = UserHistory::new();
    for _ in 0..3 {
        h.record(&[
            ("きょう".into(), "今日".into()),
            ("てんき".into(), "天気".into()),
        ]);
    }
    h
}

fn bench_convert_1best_history(c: &mut Criterion) {
    let dict = bench_dict();
    let history = bench_history();
    // Read once, outside the timed loop: the read path takes it as an argument.
    let now = lex_core::user_history::now_epoch();
    let mut group = c.benchmark_group("converter/convert_1best_history");
    for &(label, kana) in INPUTS {
        group.bench_with_input(BenchmarkId::new(label, kana.len()), &kana, |b, &kana| {
            b.iter(|| {
                lex_core::converter::ConversionContext::new(&dict, None)
                    .with_history(Some(&history), now)
                    .convert(kana)
            });
        });
    }
    group.finish();
}

fn bench_convert_10best(c: &mut Criterion) {
    let dict = bench_dict();
    let mut group = c.benchmark_group("converter/convert_10best");
    for &(label, kana) in INPUTS {
        group.bench_with_input(BenchmarkId::new(label, kana.len()), &kana, |b, &kana| {
            b.iter(|| convert_nbest(&dict, None, kana, 10));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_build_lattice,
    bench_convert_1best,
    bench_convert_1best_history,
    bench_convert_10best
);
criterion_main!(benches);
