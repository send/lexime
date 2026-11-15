//! Symbols in the candidate list's prediction (stage 3) and exact-lookup
//! (stage 4) stages: untrained symbols are not predicted, learned ones are,
//! and an exact `lookup` lists the lattice words before the symbols.

use std::collections::HashSet;
use std::sync::Arc;

use crate::converter::{build_lattice, joined_surface, ConversionContext};
use crate::dict::{CompositeDictionary, DictEntry, Dictionary, TrieDictionary};
use crate::settings::settings;
use crate::user_dict::UserDictionary;
use crate::user_history::{now_epoch, UserHistory};

use super::{generate_candidates, generate_candidates_priced, untrained_symbol};
use crate::converter::testutil::{entry_with_id, symbol_entry};

/// A symbol row; `row` is its compiled cost (its index among the reading's
/// symbol rows).
fn symbol(surface: &str, row: i16) -> DictEntry {
    DictEntry {
        left_id: 2,
        right_id: 2,
        ..symbol_entry(surface, row)
    }
}

/// Symbol rows of the synthetic dictionaries: (reading, surfaces in row order).
const SYMBOL_ROWS: &[(&str, &[&str])] = &[
    ("やじるし", &["→", "←", "↑", "↓"]),
    ("しかく", &["□", "■", "◇"]),
    ("くろぼし", &["★"]),
    ("おめが", &["Ω", "ω"]),
    ("。。", &["…"]),
];

/// A production-shaped synthetic dictionary: for each of `や` `し` `く` `お`,
/// more words under the prefix than the production list budget, some of
/// them sharing a surface, plus the symbol rows above — one of which
/// (`しかく`) shares its reading with a word, and one (Ω) its surface with
/// the word `おーむ`.
fn production_shaped_entries() -> Vec<(String, Vec<DictEntry>)> {
    let mut entries: Vec<(String, Vec<DictEntry>)> = Vec::new();
    let tails = ["あ", "い", "う", "え", "お", "か", "き", "く", "け", "こ"];
    for head in ["や", "し", "く", "お"] {
        for (i, t1) in tails.iter().enumerate() {
            for (j, t2) in tails.iter().take(4).enumerate() {
                let reading = format!("{head}{t1}{t2}");
                // Every third reading repeats an earlier surface (dedup).
                let n = i * 4 + j;
                let surface = if n % 3 == 2 {
                    format!("{head}語{}", n - 1)
                } else {
                    format!("{head}語{n}")
                };
                let cost = 3000 + (n as i16) * 37 % 500;
                entries.push((reading, vec![entry_with_id(&surface, cost, 1)]));
            }
        }
    }
    entries.push((
        "しかく".into(),
        vec![
            entry_with_id("四角", 3200, 1),
            entry_with_id("資格", 3100, 1),
        ],
    ));
    entries.push(("おーむ".into(), vec![entry_with_id("Ω", 2011, 1)]));
    entries.push(("。".into(), vec![entry_with_id("。", 1000, 1)]));
    for (reading, surfaces) in SYMBOL_ROWS {
        let rows = surfaces
            .iter()
            .enumerate()
            .map(|(i, s)| symbol(s, i as i16));
        match entries.iter_mut().find(|(r, _)| r == reading) {
            Some((_, list)) => list.extend(rows),
            None => entries.push((reading.to_string(), rows.collect())),
        }
    }
    entries
}

fn trie() -> TrieDictionary {
    TrieDictionary::from_entries(production_shaped_entries())
}

/// The production shape: the system dictionary under an (empty) user dictionary.
fn composite_of(trie: TrieDictionary) -> CompositeDictionary {
    CompositeDictionary::new(vec![Arc::new(trie), Arc::new(UserDictionary::new())])
}

/// Every proper prefix of every symbol reading the dictionary holds.
fn symbol_reading_prefixes(dict: &TrieDictionary) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (reading, entries) in dict.iter() {
        if !entries.iter().any(|e| e.symbol) {
            continue;
        }
        let chars: Vec<char> = reading.chars().collect();
        for n in 1..chars.len() {
            let p: String = chars[..n].iter().collect();
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn is_symbol(dict: &dyn Dictionary, reading: &str, surface: &str) -> bool {
    dict.lookup(reading)
        .iter()
        .any(|e| e.symbol && e.surface == surface)
}

/// with no history, the prediction stage lists no symbol for any
/// proper prefix of a symbol reading — neither in the stage's own record nor
/// in the list (lookup cannot add one: no such prefix is a symbol reading).
#[test]
fn untrained_symbols_are_not_predicted() {
    let dict = trie();
    let ctx = ConversionContext::new(&dict, None);
    let symbols: HashSet<&str> = SYMBOL_ROWS
        .iter()
        .flat_map(|(_, s)| s.iter().copied())
        .collect();
    let prefixes = symbol_reading_prefixes(&dict);
    assert!(prefixes.len() >= 8, "{prefixes:?}");
    for prefix in &prefixes {
        let priced = generate_candidates_priced(&ctx, prefix, 20);
        for p in &priced.diagnostics.predictions {
            assert!(
                !is_symbol(&dict, &p.reading, &p.surface),
                "{prefix}: untrained symbol {} predicted",
                p.surface
            );
        }
        for s in &priced.response.surfaces {
            assert!(
                !symbols.contains(s.as_str()) || s == "Ω",
                "{prefix}: symbol {s} listed"
            );
        }
    }
}

/// (Learned-punctuation caller) with history that has not learned the
/// symbol, punctuation's learned predictions exclude it too.
#[test]
fn untrained_symbols_are_not_predicted_for_punctuation() {
    let dict = trie();
    let mut h = UserHistory::new();
    let now = now_epoch();
    h.record(&[("。".into(), "。".into())]);
    let ctx = ConversionContext::new(&dict, None).with_history(Some(&h), now);
    let surfaces = generate_candidates(&ctx, "。", 20).surfaces;
    assert!(!surfaces.contains(&"…".to_string()), "{surfaces:?}");
}

/// once history has learned (reading, symbol), the symbol is predicted
/// for the reading's prefixes, first by boost order (main's behaviour).
#[test]
fn learned_symbol_is_predicted_first() {
    let dict = trie();
    let mut h = UserHistory::new();
    let now = now_epoch();
    h.record(&[("やじるし".into(), "↓".into())]);
    let ctx = ConversionContext::new(&dict, None).with_history(Some(&h), now);
    for prefix in ["や", "やじ", "やじる"] {
        let priced = generate_candidates_priced(&ctx, prefix, 20);
        let first = priced
            .diagnostics
            .predictions
            .first()
            .map(|p| p.surface.as_str());
        assert_eq!(first, Some("↓"), "{prefix}");
        assert!(
            !priced
                .diagnostics
                .predictions
                .iter()
                .any(|p| p.surface == "→"),
            "{prefix}: the untrained → stays excluded"
        );
    }

    let mut h = UserHistory::new();
    h.record(&[("。。".into(), "…".into())]);
    let ctx = ConversionContext::new(&dict, None).with_history(Some(&h), now);
    let surfaces = generate_candidates(&ctx, "。", 20).surfaces;
    assert_eq!(
        surfaces.first().map(String::as_str),
        Some("…"),
        "{surfaces:?}"
    );
}

fn lookup_order(dict: &dyn Dictionary, reading: &str) -> Vec<(String, bool)> {
    dict.lookup(reading)
        .into_iter()
        .map(|e| (e.surface, e.symbol))
        .collect()
}

/// an exact lookup lists the words (cost order) and then the symbols
/// (row order) — a word at cost 9000 before a symbol at 0.
#[test]
fn lookup_lists_words_then_symbols() {
    let make = || {
        TrieDictionary::from_entries(vec![(
            "ほし".to_string(),
            vec![
                symbol("★", 0),
                entry_with_id("星", 9000, 1),
                symbol("☆", 1),
                entry_with_id("干し", 5000, 1),
                symbol("✪", 2),
            ],
        )])
    };
    let expected: Vec<(String, bool)> = [
        ("干し", false),
        ("星", false),
        ("★", true),
        ("☆", true),
        ("✪", true),
    ]
    .iter()
    .map(|&(s, f)| (s.to_string(), f))
    .collect();
    assert_eq!(lookup_order(&make(), "ほし"), expected, "trie");
    assert_eq!(
        lookup_order(&composite_of(make()), "ほし"),
        expected,
        "composite"
    );
    // Composite over two system layers: the merge re-sorts across layers.
    let split = CompositeDictionary::new(vec![
        Arc::new(TrieDictionary::from_entries(vec![(
            "ほし".to_string(),
            vec![symbol("★", 0), symbol("☆", 1), symbol("✪", 2)],
        )])),
        Arc::new(TrieDictionary::from_entries(vec![(
            "ほし".to_string(),
            vec![entry_with_id("星", 9000, 1), entry_with_id("干し", 5000, 1)],
        )])),
    ]);
    assert_eq!(
        lookup_order(&split, "ほし"),
        expected,
        "composite of layers"
    );
}

/// (Guard of existing behaviour) a user_dict word equal to a symbol
/// row wins (user cost −1 < row index 0) — one ※ in `lookup`, the user's;
/// the lattice has its node; the list has it as a word (a path's surface).
#[test]
fn user_dict_word_wins_over_equal_symbol() {
    let sys = TrieDictionary::from_entries(vec![(
        "こめじるし".to_string(),
        vec![symbol("※", 0), symbol("＊", 1)],
    )]);
    let user = UserDictionary::new();
    user.register("こめじるし", "※");
    let dict = CompositeDictionary::new(vec![Arc::new(sys), Arc::new(user)]);

    let komes: Vec<DictEntry> = dict
        .lookup("こめじるし")
        .into_iter()
        .filter(|e| e.surface == "※")
        .collect();
    assert_eq!(komes.len(), 1);
    assert_eq!((komes[0].cost, komes[0].symbol), (-1, false));

    let lattice = build_lattice(&dict, "こめじるし");
    assert!(
        (0..lattice.node_count()).any(|i| lattice.surface(i) == "※"),
        "lattice has the user's ※"
    );

    let ctx = ConversionContext::new(&dict, None);
    let response = generate_candidates(&ctx, "こめじるし", 20);
    assert!(
        response.paths.iter().any(|p| joined_surface(p) == "※"),
        "※ is an N-best path: {:?}",
        response.surfaces
    );
}

/// Reference for the next test: the same window (`predict(prefix, 1000)`), the symbol
/// entries dropped, then cost sort → surface dedup → truncate.
fn reference_words(dict: &dyn Dictionary, prefix: &str, max: usize) -> Vec<(String, String, i16)> {
    let mut flat: Vec<(String, DictEntry)> = Vec::new();
    for sr in dict.predict(prefix, 1000) {
        for e in sr.entries {
            if !e.symbol {
                flat.push((sr.reading.clone(), e));
            }
        }
    }
    flat.sort_by_key(|(_, e)| e.cost);
    let mut seen = HashSet::new();
    flat.retain(|(_, e)| seen.insert(e.surface.clone()));
    flat.truncate(max);
    flat.into_iter()
        .map(|(r, e)| (r, e.surface, e.cost))
        .collect()
}

fn predicted_words(dict: &dyn Dictionary, prefix: &str, max: usize) -> Vec<(String, String, i16)> {
    dict.predict_ranked(prefix, max, 1000, &untrained_symbol(None))
        .into_iter()
        .filter(|(_, e)| !e.symbol)
        .map(|(r, e)| (r, e.surface, e.cost))
        .collect()
}

fn assert_words_match_reference(trie: TrieDictionary, prefixes: &[String]) {
    let max = settings().candidates.max_results;
    for prefix in prefixes {
        assert_eq!(
            predicted_words(&trie, prefix, max),
            reference_words(&trie, prefix, max),
            "trie {prefix}"
        );
    }
    let composite = composite_of(trie);
    for prefix in prefixes {
        assert_eq!(
            predicted_words(&composite, prefix, max),
            reference_words(&composite, prefix, max),
            "composite {prefix}"
        );
    }
}

fn t_b7_prefixes(dict: &TrieDictionary) -> Vec<String> {
    let mut prefixes: Vec<String> = ["や", "し", "く", "お"].map(String::from).to_vec();
    for p in symbol_reading_prefixes(dict) {
        if !prefixes.contains(&p) {
            prefixes.push(p);
        }
    }
    prefixes
}

/// with no history, the words predicted for every prefix with symbol
/// rows below it are the reference list byte for byte — the same window with
/// the symbols dropped before ranking. Trie alone and under user_dict.
#[test]
fn predicted_words_match_symbol_free_reference() {
    let dict = trie();
    let prefixes = t_b7_prefixes(&dict);
    // The fixture must fill the list budget (so dropped symbols would cost
    // slots) and hold a word sharing a symbol's surface.
    let max = settings().candidates.max_results;
    assert_eq!(predicted_words(&dict, "や", max).len(), max);
    assert_words_match_reference(dict, &prefixes);
}

/// The same, on the compiled production dictionary (`mise run dict`; path from
/// `LEXIME_DICT`, default `engine/data/lexime.dict`). Run with `--ignored`.
#[test]
#[ignore]
fn predicted_words_match_symbol_free_reference_real_dict() {
    let path = std::env::var("LEXIME_DICT").map_or_else(
        |_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/lexime.dict"),
        std::path::PathBuf::from,
    );
    if !path.exists() {
        eprintln!("skipping: dict not found at {path:?}");
        return;
    }
    let dict = TrieDictionary::open(&path).unwrap();
    let prefixes = t_b7_prefixes(&dict);
    assert!(
        prefixes.len() > 4,
        "the dictionary has no symbols: {path:?}"
    );
    assert_words_match_reference(dict, &prefixes);
}

/// the symbol (`おめが`, Ω) is dropped before the surface dedup, so it
/// does not hide the word (`おーむ`, Ω) from `お`'s prediction.
#[test]
fn excluded_symbol_does_not_shadow_word_of_same_surface() {
    let has_omega = |dict: &dyn Dictionary| {
        dict.predict_ranked("お", 1000, 1000, &untrained_symbol(None))
            .iter()
            .any(|(r, e)| r == "おーむ" && e.surface == "Ω" && !e.symbol)
    };
    assert!(has_omega(&trie()), "trie");
    assert!(has_omega(&composite_of(trie())), "composite");
}
