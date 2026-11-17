//! The Latin-unit formation rule (R-v): a counter written in Latin letters
//! only is entered only from a typed ASCII digit — in Viterbi's BOS
//! initialisation and forward pass (1-best and N-best), on a lattice built or
//! extended, and in resegment's two new adjacencies.

use crate::converter::cost::DefaultCostFunction;
use crate::converter::latin_unit::{
    is_latin_unit, latin_unit_admissible, pure_latin, segment_is_latin_unit, segment_typed_digit,
    typed_digit,
};
use crate::converter::lattice::{build_lattice, Lattice};
use crate::converter::resegment::resegment;
use crate::converter::testutil::{entry, entry_with_id, zero_conn_with_fw_roles};
use crate::converter::viterbi::{viterbi_nbest, ScoredPath};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;

/// POS ids of the fixture.
const COUNTER: u16 = 10;
const CONTENT: u16 = 1;
const NUMERAL: u16 = 20;
const DIGIT_WORD: u16 = 22; // reading に / さん, surface 2 / 3 (Mozc's 2044 shape)
const PREFIX_SYMBOL: u16 = 40; // μ
const SYMBOL: u16 = 50; // n (記号)
const PARTICLE: u16 = 55; // function word

fn conn() -> ConnectionMatrix {
    let mut roles = vec![0u8; 60];
    roles[COUNTER as usize] = 7;
    roles[PARTICLE as usize] = 1;
    zero_conn_with_fw_roles(60, PARTICLE, PARTICLE, roles)
}

/// Latin units are the cheapest reading of their kana, so a path takes one
/// wherever the rule lets it.
fn dict() -> TrieDictionary {
    TrieDictionary::from_entries(vec![
        (
            "びょう".into(),
            vec![
                entry_with_id("s", 100, COUNTER),
                entry_with_id("秒", 3000, COUNTER),
            ],
        ),
        (
            "さん".into(),
            vec![
                entry_with_id("三", 500, NUMERAL),
                entry_with_id("3", 900, DIGIT_WORD),
            ],
        ),
        ("に".into(), vec![entry_with_id("2", 500, DIGIT_WORD)]),
        ("この".into(), vec![entry_with_id("この", 500, CONTENT)]),
        ("きろ".into(), vec![entry_with_id("キロ", 500, CONTENT)]),
        (
            "りっとる".into(),
            vec![
                entry_with_id("L", 100, COUNTER),
                entry_with_id("リットル", 3000, COUNTER),
            ],
        ),
        (
            "まいくろ".into(),
            vec![entry_with_id("μ", 500, PREFIX_SYMBOL)],
        ),
        (
            "きろめーとるさき".into(),
            vec![entry_with_id("km先", 100, COUNTER)],
        ),
        ("えぬ".into(), vec![entry_with_id("n", 100, SYMBOL)]),
        (
            "めびばいと".into(),
            vec![entry_with_id("MiB", 100, COUNTER)],
        ),
        (
            "だいじょうぶ".into(),
            vec![entry_with_id("大丈夫", 500, CONTENT)],
        ),
        ("の".into(), vec![entry_with_id("の", 100, PARTICLE)]),
        ("を".into(), vec![entry_with_id("を", 100, PARTICLE)]),
        // One-segment compounds resegment splits (to の|s and s|を).
        ("のびょう".into(), vec![entry_with_id("野秒", 10, CONTENT)]),
        ("びょうを".into(), vec![entry_with_id("鋲緒", 10, CONTENT)]),
        ("ほ".into(), vec![entry("帆", 100)]),
    ])
}

/// Every path's segments as (surface, …) for the N-best (n = 50) and the
/// 1-best on `lattice`.
fn paths(lattice: &Lattice, conn: Option<&ConnectionMatrix>, n: usize) -> Vec<Vec<String>> {
    let cost_fn = DefaultCostFunction::new(conn);
    surfaces(&viterbi_nbest(lattice, &cost_fn, n))
}

/// Each path's segment surfaces.
fn surfaces(paths: &[ScoredPath]) -> Vec<Vec<String>> {
    paths
        .iter()
        .map(|p| p.segments.iter().map(|s| s.surface.clone()).collect())
        .collect()
}

/// `pair` (two adjacent surfaces) is on some path.
fn has_pair(paths: &[Vec<String>], pair: [&str; 2]) -> bool {
    paths
        .iter()
        .any(|p| p.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1]))
}

/// `surface` is the first segment of some path.
fn has_head(paths: &[Vec<String>], surface: &str) -> bool {
    paths
        .iter()
        .any(|p| p.first().is_some_and(|s| s == surface))
}

/// Check one reading in both widths: N-best (n = 50) and 1-best.
fn check(reading: &str, conn: Option<&ConnectionMatrix>, f: impl Fn(&[Vec<String>], usize)) {
    let lattice = build_lattice(&dict(), reading);
    for n in [50, 1] {
        f(&paths(&lattice, conn, n), n);
    }
}

#[test]
fn latin_unit_forms_after_a_typed_digit() {
    let c = conn();
    check("3びょう", Some(&c), |p, n| {
        assert_eq!(p[0], ["3", "s"], "n={n}");
    });
    check("512めびばいと", Some(&c), |p, n| {
        assert_eq!(p[0], ["5", "1", "2", "MiB"], "n={n}");
    });
}

#[test]
fn latin_unit_does_not_form_after_anything_else() {
    let c = conn();
    for (reading, prev) in [
        ("さんびょう", "三"),
        ("さんびょう", "3"), // reading さん: not typed
        ("にびょう", "2"),   // reading に (Mozc 2044 shape)
        ("このびょう", "この"),
        ("まいくろびょう", "μ"),
    ] {
        check(reading, Some(&c), |p, n| {
            assert!(!has_pair(p, [prev, "s"]), "{reading} n={n}: {p:?}");
            assert!(!p.is_empty(), "{reading} n={n}");
        });
    }
    check("きろりっとる", Some(&c), |p, n| {
        assert!(!has_pair(p, ["キロ", "L"]), "n={n}: {p:?}");
        assert!(has_pair(p, ["キロ", "リットル"]), "n={n}: {p:?}");
    });
}

#[test]
fn latin_unit_is_not_formed_from_bos() {
    let c = conn();
    check("びょう", Some(&c), |p, n| {
        assert!(!has_head(p, "s"), "n={n}: {p:?}");
        assert_eq!(p[0], ["秒"], "n={n}");
    });
}

#[test]
fn a_lattice_whose_only_path_needs_an_unformed_unit_has_no_path() {
    // The rule only narrows predecessors: a node left with none is
    // unreachable, and Viterbi returns no path (no panic).
    let d = TrieDictionary::from_entries(vec![(
        "びょう".into(),
        vec![entry_with_id("s", 100, COUNTER)],
    )]);
    let c = conn();
    let lattice = build_lattice(&d, "びょう");
    let only = Lattice::from_test_nodes("びょう", &[(0, 3, "びょう", "s", 100, COUNTER, COUNTER)]);
    assert!(paths(&only, Some(&c), 5).is_empty());
    // The built lattice keeps the fallback path (connectivity).
    assert!(!paths(&lattice, Some(&c), 5).is_empty());
}

#[test]
fn mixed_counters_and_non_counter_latin_are_outside_the_rule() {
    let c = conn();
    // km先 (mixed) and n (記号) form after anything.
    check("3きろめーとるさき", Some(&c), |p, n| {
        assert!(has_pair(p, ["3", "km先"]), "n={n}: {p:?}");
    });
    check("このきろめーとるさき", Some(&c), |p, n| {
        assert!(has_pair(p, ["この", "km先"]), "n={n}: {p:?}");
    });
    check("3えぬ", Some(&c), |p, n| {
        assert!(has_pair(p, ["3", "n"]), "n={n}: {p:?}");
    });
    check("このえぬ", Some(&c), |p, n| {
        assert!(has_pair(p, ["この", "n"]), "n={n}: {p:?}");
    });
}

#[test]
fn a_typed_latin_fallback_is_not_a_latin_unit() {
    // `n` typed as is: the 1-char fallback (id 0, content word) is pure
    // Latin but not a counter, so the rule leaves it alone.
    let c = conn();
    check("だいじょうぶn", Some(&c), |p, n| {
        assert_eq!(p[0], ["大丈夫", "n"], "n={n}");
    });
}

#[test]
fn without_a_connection_matrix_the_rule_is_off() {
    for (reading, pair) in [
        ("さんびょう", ["三", "s"]),
        ("このびょう", ["この", "s"]),
        ("3びょう", ["3", "s"]),
    ] {
        check(reading, None, |p, n| {
            if n > 1 {
                assert!(has_pair(p, pair), "{reading}: {p:?}");
            }
        });
    }
    check("びょう", None, |p, _| assert!(has_head(p, "s"), "{p:?}"));
}

#[test]
fn extended_lattice_carries_the_same_flags_and_paths() {
    let d = dict();
    let c = conn();
    for (start, full) in [
        ("3", "3びょう"),
        ("さ", "さんびょう"),
        ("この", "このびょう"),
    ] {
        let built = build_lattice(&d, full);
        let mut extended = build_lattice(&d, start);
        // One char at a time, as the session's LatticeCache does.
        let chars: Vec<char> = full.chars().collect();
        for k in start.chars().count() + 1..=chars.len() {
            let prefix: String = chars[..k].iter().collect();
            extended.extend(&d, &prefix);
        }
        for idx in 0..extended.node_count() {
            assert_eq!(
                extended.pure_latin(idx),
                pure_latin(extended.surface(idx)),
                "{full} {}",
                extended.surface(idx)
            );
            assert_eq!(
                extended.typed_digit(idx),
                typed_digit(extended.reading(idx), extended.surface(idx)),
                "{full} {}",
                extended.surface(idx)
            );
        }
        for n in [50, 1] {
            assert_eq!(
                paths(&built, Some(&c), n),
                paths(&extended, Some(&c), n),
                "{full}"
            );
        }
    }
}

fn reseg(reading: &str, c: &ConnectionMatrix) -> Vec<Vec<String>> {
    let lattice = build_lattice(&dict(), reading);
    let cost_fn = DefaultCostFunction::new(Some(c));
    let best: Vec<ScoredPath> = viterbi_nbest(&lattice, &cost_fn, 1);
    surfaces(&resegment(&best, &lattice, Some(c)))
}

#[test]
fn resegment_does_not_form_a_latin_unit_after_a_non_digit() {
    let c = conn();
    // Left → right: 野秒 split into の|s (の is not a typed digit).
    let p = reseg("のびょう", &c);
    assert!(!has_pair(&p, ["の", "s"]), "{p:?}");
    assert!(has_pair(&p, ["の", "秒"]), "{p:?}");
    // Previous segment → left: この|鋲緒 split into この|s|を.
    let p = reseg("このびょうを", &c);
    assert!(!has_pair(&p, ["この", "s"]), "{p:?}");
    // BOS → left: 鋲緒 split into s|を.
    let p = reseg("びょうを", &c);
    assert!(!has_head(&p, "s"), "{p:?}");
    // Control: after a typed digit the split is made.
    let p = reseg("3びょうを", &c);
    assert!(has_pair(&p, ["3", "s"]), "{p:?}");
}

/// S1: the lattice's SoA flags and a path segment's flags (the
/// `RichSegment` route features use) agree on every predicate.
#[test]
fn soa_and_segment_routes_agree() {
    let c = conn();
    let rows: &[(&str, &str, u16)] = &[
        ("びょう", "s", COUNTER),
        ("まいくろびょう", "μs", COUNTER),
        ("きろめーとるさき", "km先", COUNTER),
        ("ぱーせんと", "%", COUNTER),
        ("おーむ", "Ω", COUNTER),
        ("3", "3", 0),
        ("３", "３", 0),
        ("えぬ", "n", SYMBOL),
        ("に", "2", DIGIT_WORD),
        ("n", "n", 0),
    ];
    let nodes: Vec<(usize, usize, &str, &str, i16, u16, u16)> = rows
        .iter()
        .map(|&(r, s, id)| (0, 1, r, s, 0, id, id))
        .collect();
    let lattice = Lattice::from_test_nodes("x", &nodes);
    for conn in [Some(&c), None] {
        for idx in 0..lattice.node_count() {
            let seg = lattice.to_rich_segment(idx);
            let soa_unit = is_latin_unit(lattice.pure_latin(idx), lattice.left_id(idx), conn);
            assert_eq!(
                soa_unit,
                segment_is_latin_unit(&seg, conn),
                "{}",
                seg.surface
            );
            assert_eq!(
                lattice.typed_digit(idx),
                segment_typed_digit(&seg),
                "{}",
                seg.surface
            );
            for prev in 0..lattice.node_count() {
                let pseg = lattice.to_rich_segment(prev);
                assert_eq!(
                    latin_unit_admissible(lattice.typed_digit(prev), soa_unit),
                    latin_unit_admissible(
                        segment_typed_digit(&pseg),
                        segment_is_latin_unit(&seg, conn)
                    ),
                    "{} after {}",
                    seg.surface,
                    pseg.surface
                );
            }
        }
    }
    // The table's expected values.
    let unit = |s: &str| {
        let idx = (0..lattice.node_count())
            .find(|&i| lattice.surface(i) == s && lattice.reading(i) != "n")
            .unwrap();
        is_latin_unit(lattice.pure_latin(idx), lattice.left_id(idx), Some(&c))
    };
    assert!(unit("s") && unit("μs"));
    assert!(!unit("km先") && !unit("%") && !unit("Ω") && !unit("3") && !unit("n"));
}

/// R-a' in the features rerank prices with (E2): a formed `3`+`s` is priced
/// as katakana, a hand-built `三`+`s` (the rule bypassed) falls back to
/// `latin_penalty` and is counted.
#[test]
fn features_price_formed_and_unformed_latin_units() {
    use crate::converter::features::FeatureConfig;
    use crate::converter::viterbi::{PathOrigin, RichSegment};
    let c = conn();
    let seg = |reading: &str, surface: &str, id: u16| RichSegment {
        reading: reading.into(),
        surface: surface.into(),
        left_id: id,
        right_id: id,
        word_cost: 0,
    };
    let fcfg = FeatureConfig {
        conn: Some(&c),
        dict: None,
    };
    let cost = &crate::settings::settings().cost;
    let formed = ScoredPath::new(
        vec![seg("3", "3", 0), seg("びょう", "s", COUNTER)],
        0,
        PathOrigin::Viterbi,
    );
    let f = fcfg.extract(&formed);
    assert_eq!(
        (f.script_cost, f.latin_unit_unformed),
        (cost.katakana_penalty, 0)
    );
    let unformed = ScoredPath::new(
        vec![seg("さん", "三", NUMERAL), seg("びょう", "s", COUNTER)],
        0,
        PathOrigin::Viterbi,
    );
    let f = fcfg.extract(&unformed);
    let kanji = -cost.pure_kanji_bonus * 2 / 3;
    assert_eq!(
        (f.script_cost, f.latin_unit_unformed),
        (kanji + cost.latin_penalty, 1)
    );
}
