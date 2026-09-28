//! Variant offers through the whole pipeline: priced the same whatever the
//! oversample, learnable like any path, and never carrying a source's boost.

use super::*;
use crate::converter::viterbi::PathOrigin;
use crate::dict::{DictEntry, TrieDictionary};
use crate::user_history::UserHistory;

/// あった|ほう|が, where the dictionary prefers kana ほう (0) to 方 (6000):
/// あった|方|が is a real lattice path, but dearer than its kana source by
/// more than the offer cap, so the offer (source + 2000) prices it. 会った
/// and 蛾 give cheaper paths, so a narrow oversample leaves the 方 path out.
fn attahouga() -> TrieDictionary {
    let e = |surface: &str, cost: i16| DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    };
    TrieDictionary::from_entries(vec![
        ("あった".into(), vec![e("あった", 0), e("会った", 50)]),
        ("ほう".into(), vec![e("ほう", 0), e("方", 6000)]),
        ("が".into(), vec![e("が", 0), e("蛾", 100)]),
    ])
}

fn ctx<'a>(dict: &'a TrieDictionary, history: Option<&'a UserHistory>) -> ConversionContext<'a> {
    ConversionContext {
        dict,
        conn: None,
        history,
    }
}

fn price_of(paths: &[ScoredPath], s: &str) -> Option<i64> {
    paths
        .iter()
        .find(|p| p.surface_key() == s)
        .map(|p| p.pre_history_cost())
}

#[test]
fn offer_price_does_not_depend_on_the_oversample() {
    let dict = attahouga();
    let c = ctx(&dict, None);
    let lattice = c.build_lattice("あったほうが");
    // Oversample 2 leaves the real あった|方|が path out; 20 holds it. (At
    // oversample 1 rerank sees one path and applies no features, so every
    // price differs — not an offer property.)
    let narrow = c.convert_lattice_impl(&lattice, 20, 2);
    let wide = c.convert_lattice_impl(&lattice, 20, 20);
    let cost_fn = DefaultCostFunction::new(None);
    let raw = |k| -> Vec<String> {
        viterbi_nbest(&lattice, &cost_fn, k)
            .iter()
            .map(ScoredPath::surface_key)
            .collect()
    };
    assert!(!raw(2).contains(&"あった方が".to_string()));
    assert!(raw(20).contains(&"あった方が".to_string()));
    let (n, w) = (
        price_of(&narrow, "あった方が"),
        price_of(&wide, "あった方が"),
    );
    assert!(
        n.is_some(),
        "offered even when the real path is outside the oversample"
    );
    assert_eq!(n, w);
    // The cap binds: offered within 2000 of the kana path, far below the
    // model's 6000-dearer kanji spelling.
    let kana = price_of(&wide, "あったほうが").unwrap();
    assert!(n.unwrap() <= kana + 2000, "{n:?} vs kana {kana}");
}

#[test]
fn one_best_returns_a_learned_offer() {
    let dict = attahouga();
    let mut h = UserHistory::new();
    for _ in 0..3 {
        h.record(&[("あったほうが".into(), "あった方が".into())]);
    }
    let c = ctx(&dict, Some(&h));
    let lattice = c.build_lattice("あったほうが");
    for (n, oversample) in [(1, 2), (1, 30), (20, 60)] {
        let top = c.convert_lattice_impl(&lattice, n, oversample);
        assert_eq!(
            top[0].surface_key(),
            "あった方が",
            "n={n} oversample={oversample}"
        );
    }
}

#[test]
fn offer_carries_no_boost_from_its_source() {
    // #248: learning the kana path must not lower its variant's price.
    let dict = attahouga();
    let mut h = UserHistory::new();
    for _ in 0..5 {
        h.record(&[("あったほうが".into(), "あったほうが".into())]);
    }
    let plain = ctx(&dict, None);
    let learned = ctx(&dict, Some(&h));
    let lattice = plain.build_lattice("あったほうが");
    let before = plain.convert_lattice_impl(&lattice, 20, 20);
    let after = learned.convert_lattice_impl(&lattice, 20, 60);
    let v = after
        .iter()
        .find(|p| p.surface_key() == "あった方が")
        .unwrap();
    assert_eq!(v.history_boost, 0);
    assert_eq!(Some(v.viterbi_cost), price_of(&before, "あった方が"));
}

#[test]
fn offers_do_not_use_up_the_n_budget() {
    // With n = 4 the list keeps the four cheapest model-priced paths, plus
    // every offer sorted among them: an offer above the 4th model path
    // (会った|方|蛾 above あった|ほう|が) does not push it off.
    let dict = attahouga();
    let c = ctx(&dict, None);
    let lattice = c.build_lattice("あったほうが");
    let all = c.convert_lattice_impl(&lattice, 20, 20);
    let model: Vec<String> = all
        .iter()
        .filter(|p| p.priced_by.is_model())
        .map(ScoredPath::surface_key)
        .take(4)
        .collect();
    let top = c.convert_lattice_impl(&lattice, 4, 20);
    let top_model: Vec<String> = top
        .iter()
        .filter(|p| p.priced_by.is_model())
        .map(ScoredPath::surface_key)
        .collect();
    assert_eq!(top_model, model);
    let fourth = top
        .iter()
        .position(|p| p.surface_key() == model[3])
        .unwrap();
    assert!(
        top[..fourth].iter().any(|p| !p.priced_by.is_model()),
        "fixture: an offer sorts above the 4th model path"
    );
    // Past the n-th model path only what the Override stage adds after the
    // cut (Katakana / Numeric).
    assert!(top[fourth + 1..]
        .iter()
        .all(|p| matches!(p.origin, PathOrigin::Katakana | PathOrigin::Numeric)));
}

/// attahouga plus 遭った and 画, for a history that lifts 遭った paths above
/// the pre-history best.
fn attahouga_met() -> TrieDictionary {
    let e = |surface: &str, cost: i16| DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    };
    TrieDictionary::from_entries(vec![
        (
            "あった".into(),
            vec![e("あった", 0), e("会った", 50), e("遭った", 200)],
        ),
        ("ほう".into(), vec![e("ほう", 0), e("方", 6000)]),
        ("が".into(), vec![e("が", 0), e("蛾", 100), e("画", 150)]),
    ])
}

#[test]
fn restoring_the_pre_history_best_keeps_the_offers_riding_in_the_window() {
    // History moves the pre-history best out of the first n model paths.
    // It comes back in place of the n-th model path; the offers sorted
    // before that path are not cut with it.
    let dict = attahouga_met();
    let mut h = UserHistory::new();
    for _ in 0..5 {
        h.record(&[("あった".into(), "遭った".into())]);
    }
    h.record(&[("ほう".into(), "方".into())]);
    let c = ctx(&dict, Some(&h));
    let lattice = c.build_lattice("あったほうが");
    let n = 4;
    let all = c.convert_lattice_impl(&lattice, 50, 60);
    let top = c.convert_lattice_impl(&lattice, n, 60);
    let best = ctx(&dict, None).convert_lattice_impl(&lattice, 1, 60)[0].surface_key();
    let surfaces: Vec<String> = top.iter().map(ScoredPath::surface_key).collect();
    assert!(
        surfaces.contains(&best),
        "the pre-history best is restored: {surfaces:?}"
    );
    // The offers the full list sorts before its n-th model path (the one
    // the best replaces) stay.
    let cut = all
        .iter()
        .enumerate()
        .filter(|(_, p)| p.priced_by.is_model() && p.surface_key() != best)
        .nth(n - 1)
        .map(|(i, _)| i)
        .unwrap();
    let riding: Vec<String> = all[..cut]
        .iter()
        .filter(|p| !p.priced_by.is_model())
        .map(ScoredPath::surface_key)
        .collect();
    assert!(!riding.is_empty(), "fixture: offers ride in the window");
    for s in &riding {
        assert!(surfaces.contains(s), "{s} was cut: {surfaces:?}");
    }
    assert_eq!(top.iter().filter(|p| p.priced_by.is_model()).count(), n);
}

#[test]
fn one_best_with_an_empty_history_equals_one_best_without() {
    let dict = attahouga_met();
    let empty = UserHistory::new();
    let lattice = ctx(&dict, None).build_lattice("あったほうが");
    for oversample in [2, 30] {
        let without = ctx(&dict, None).convert_lattice_impl(&lattice, 1, oversample);
        let with = ctx(&dict, Some(&empty)).convert_lattice_impl(&lattice, 1, oversample);
        assert_eq!(
            with.iter().map(ScoredPath::surface_key).collect::<Vec<_>>(),
            without
                .iter()
                .map(ScoredPath::surface_key)
                .collect::<Vec<_>>()
        );
        assert_eq!(with[0].viterbi_cost, without[0].viterbi_cost);
    }
}
