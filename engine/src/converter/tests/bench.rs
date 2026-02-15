use super::*;
use crate::dict::{DictEntry, TrieDictionary};

/// Build a richer dictionary for benchmarking that covers all words in the
/// benchmark inputs so we exercise real dictionary lookup paths, not just
/// unknown-word fallback.
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
                },
                DictEntry {
                    surface: "京".into(),
                    cost: 5000,
                    left_id: 101,
                    right_id: 101,
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
                },
                DictEntry {
                    surface: "いい".into(),
                    cost: 4000,
                    left_id: 301,
                    right_id: 301,
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
            }],
        ),
        (
            "です".into(),
            vec![DictEntry {
                surface: "です".into(),
                cost: 2500,
                left_id: 800,
                right_id: 800,
            }],
        ),
        (
            "ね".into(),
            vec![DictEntry {
                surface: "ね".into(),
                cost: 2000,
                left_id: 900,
                right_id: 900,
            }],
        ),
        (
            "わたし".into(),
            vec![DictEntry {
                surface: "私".into(),
                cost: 3000,
                left_id: 1000,
                right_id: 1000,
            }],
        ),
        (
            "だ".into(),
            vec![DictEntry {
                surface: "だ".into(),
                cost: 2500,
                left_id: 810,
                right_id: 810,
            }],
        ),
        (
            "と".into(),
            vec![DictEntry {
                surface: "と".into(),
                cost: 2000,
                left_id: 820,
                right_id: 820,
            }],
        ),
        (
            "おもい".into(),
            vec![DictEntry {
                surface: "思い".into(),
                cost: 3500,
                left_id: 830,
                right_id: 830,
            }],
        ),
        (
            "おもいます".into(),
            vec![DictEntry {
                surface: "思います".into(),
                cost: 3200,
                left_id: 831,
                right_id: 831,
            }],
        ),
        (
            "ます".into(),
            vec![DictEntry {
                surface: "ます".into(),
                cost: 2500,
                left_id: 840,
                right_id: 840,
            }],
        ),
        (
            "い".into(),
            vec![DictEntry {
                surface: "胃".into(),
                cost: 6000,
                left_id: 600,
                right_id: 600,
            }],
        ),
        (
            "き".into(),
            vec![DictEntry {
                surface: "木".into(),
                cost: 4500,
                left_id: 500,
                right_id: 500,
            }],
        ),
        (
            "てん".into(),
            vec![DictEntry {
                surface: "天".into(),
                cost: 5000,
                left_id: 700,
                right_id: 700,
            }],
        ),
        (
            "がくせい".into(),
            vec![DictEntry {
                surface: "学生".into(),
                cost: 4000,
                left_id: 1100,
                right_id: 1100,
            }],
        ),
        (
            "しゅくだい".into(),
            vec![DictEntry {
                surface: "宿題".into(),
                cost: 4000,
                left_id: 1200,
                right_id: 1200,
            }],
        ),
        (
            "を".into(),
            vec![DictEntry {
                surface: "を".into(),
                cost: 2000,
                left_id: 210,
                right_id: 210,
            }],
        ),
        (
            "やる".into(),
            vec![DictEntry {
                surface: "やる".into(),
                cost: 3500,
                left_id: 850,
                right_id: 850,
            }],
        ),
        (
            "の".into(),
            vec![DictEntry {
                surface: "の".into(),
                cost: 2000,
                left_id: 220,
                right_id: 220,
            }],
        ),
        (
            "が".into(),
            vec![DictEntry {
                surface: "が".into(),
                cost: 2000,
                left_id: 230,
                right_id: 230,
            }],
        ),
        (
            "めんどう".into(),
            vec![DictEntry {
                surface: "面倒".into(),
                cost: 4500,
                left_id: 860,
                right_id: 860,
            }],
        ),
        (
            "くさい".into(),
            vec![DictEntry {
                surface: "臭い".into(),
                cost: 5000,
                left_id: 870,
                right_id: 870,
            }],
        ),
        (
            "めんどうくさい".into(),
            vec![DictEntry {
                surface: "面倒くさい".into(),
                cost: 3800,
                left_id: 861,
                right_id: 861,
            }],
        ),
        (
            "けど".into(),
            vec![DictEntry {
                surface: "けど".into(),
                cost: 2500,
                left_id: 880,
                right_id: 880,
            }],
        ),
        (
            "がんばり".into(),
            vec![DictEntry {
                surface: "頑張り".into(),
                cost: 4000,
                left_id: 890,
                right_id: 890,
            }],
        ),
        (
            "がんばります".into(),
            vec![DictEntry {
                surface: "頑張ります".into(),
                cost: 3500,
                left_id: 891,
                right_id: 891,
            }],
        ),
    ];
    TrieDictionary::from_entries(entries)
}

#[test]
#[ignore]
fn bench_convert_latency() {
    let dict = bench_dict();

    let inputs: Vec<(&str, &str)> = vec![
        ("short", "きょう"),
        ("medium", "きょうはいいてんきですね"),
        ("long", "わたしはきょうはいいてんきだとおもいます"),
        (
            "very_long",
            "わたしはきょうしゅくだいをやるのがめんどうくさいけどがんばります",
        ),
    ];

    let warmup = 50;
    let iterations = 200;

    println!();
    println!("=== Viterbi Convert Pipeline Latency Benchmark ===");
    println!("  warmup: {warmup} iterations, measured: {iterations} iterations");
    println!();

    for (label, kana) in &inputs {
        let char_count = kana.chars().count();

        // Warmup
        for _ in 0..warmup {
            let _ = convert(&dict, None, kana);
        }

        // Measure convert (1-best)
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = convert(&dict, None, kana);
        }
        let elapsed_1best = start.elapsed();
        let avg_1best_us = elapsed_1best.as_micros() as f64 / iterations as f64;

        // Measure convert_nbest (10-best)
        for _ in 0..warmup {
            let _ = convert_nbest(&dict, None, kana, 10);
        }
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = convert_nbest(&dict, None, kana, 10);
        }
        let elapsed_nbest = start.elapsed();
        let avg_nbest_us = elapsed_nbest.as_micros() as f64 / iterations as f64;

        // Measure build_lattice only
        for _ in 0..warmup {
            let _ = build_lattice(&dict, kana);
        }
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = build_lattice(&dict, kana);
        }
        let elapsed_lattice = start.elapsed();
        let avg_lattice_us = elapsed_lattice.as_micros() as f64 / iterations as f64;

        println!("  {label} ({char_count} chars): \"{}\"", kana);
        println!("    build_lattice:    {:>8.1} us", avg_lattice_us);
        println!(
            "    convert (1-best): {:>8.1} us ({:.3} ms)",
            avg_1best_us,
            avg_1best_us / 1000.0
        );
        println!(
            "    convert (10-best):{:>8.1} us ({:.3} ms)",
            avg_nbest_us,
            avg_nbest_us / 1000.0
        );
        println!();
    }

    println!("=== Summary ===");
    println!("  Target: < 10ms per keystroke for responsive IME input");
    println!("  Note: using small test dictionary; real dictionary will be larger");
    println!("        but trie lookups are O(key_length), not O(dict_size).");
}
