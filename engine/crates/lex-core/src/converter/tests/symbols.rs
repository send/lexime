//! Symbol placement (`place_symbols`, plan §4-7-3 P-s): a reading's symbol
//! entries are inserted by Mozc's position rule over the population
//! admission keeps by price, copying the price before them; admission
//! exempts them; later stages only move an unlearned symbol down (I9″).

use super::*;
use crate::converter::postprocess::{
    admit_by_cost_gap, place_symbols, postprocess_observed, PostprocessContext,
    PostprocessObserver, SymbolCandidate,
};
use crate::converter::reranker::history_rerank_at;
use crate::converter::testutil::{entry, entry_with_id, learned, T0};
use crate::converter::viterbi::{PathOrigin, RichSegment};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::{CompositeDictionary, DictEntry, Dictionary, TrieDictionary};
use crate::settings::DEFAULT_CANDIDATE_MAX_COST_GAP;
use crate::user_dict::UserDictionary;
use crate::user_history::UserHistory;

const R: &str = "よみ";
/// The anchor's admission limit at the default T: anchor + 8000.
const GAP: i64 = DEFAULT_CANDIDATE_MAX_COST_GAP;

/// A symbol row; `row` is its compiled cost (its index among the reading's
/// symbol rows).
fn symbol(surface: &str, row: i16) -> DictEntry {
    DictEntry {
        surface: surface.into(),
        cost: row,
        left_id: 2,
        right_id: 3,
        symbol: true,
    }
}

/// A dictionary with only `symbols` (row order) under [`R`].
fn symbols_dict(symbols: &[&str]) -> TrieDictionary {
    TrieDictionary::from_entries([(
        R.to_string(),
        symbols
            .iter()
            .enumerate()
            .map(|(i, s)| symbol(s, i as i16))
            .collect(),
    )])
}

/// A one-segment model path of [`R`].
fn path(surface: &str, cost: i64) -> ScoredPath {
    ScoredPath::single(R.into(), surface.into(), cost, PathOrigin::Viterbi)
}

/// The typed kana.
fn identity(cost: i64) -> ScoredPath {
    path(R, cost)
}

fn place(paths: &mut Vec<ScoredPath>, dict: &dyn Dictionary) -> Vec<SymbolCandidate> {
    place_symbols(paths, dict, R, GAP)
}

fn keys(paths: &[ScoredPath]) -> Vec<String> {
    paths.iter().map(ScoredPath::surface_key).collect()
}

fn index_of(paths: &[ScoredPath], surface: &str) -> Option<usize> {
    paths.iter().position(|p| p.surface_key_eq(surface))
}

/// What follows the placement in the pipeline up to the cut: history's
/// stable cost sort (with `history`, or an empty one) and admission at the
/// pre-history anchor.
fn downstream(paths: &mut Vec<ScoredPath>, history: Option<&UserHistory>) {
    let anchor = paths[0].viterbi_cost;
    let empty = UserHistory::new();
    history_rerank_at(paths, history.unwrap_or(&empty), None, T0);
    admit_by_cost_gap(paths, anchor, GAP, |_| {});
}

/// `n` two-kanji model paths at 1000, 1100, …
fn words(n: usize) -> Vec<ScoredPath> {
    const SURFACES: [&str; 16] = [
        "一二", "三四", "五六", "七八", "九十", "百千", "万億", "東西", "南北", "上下", "左右",
        "前後", "内外", "天地", "山川", "草木",
    ];
    (0..n)
        .map(|i| path(SURFACES[i], 1000 + 100 * i as i64))
        .collect()
}

// ---------------------------------------------------------------------------
// The position rule and the copied price
// ---------------------------------------------------------------------------

/// (1) Six paths, none stepped over: the symbol goes to index 3 at the
/// price of index 2; indexes 0–2 and the anchor are untouched, and the
/// position holds through history's stable sort.
#[test]
fn a_symbol_goes_to_index_3_at_the_price_before_it() {
    let dict = symbols_dict(&["※"]);
    let mut paths = words(6);
    let before = keys(&paths[..3]);
    let placed = place(&mut paths, &dict);
    assert_eq!(keys(&paths[..3]), before);
    assert_eq!(paths[0].viterbi_cost, 1000, "anchor");
    assert_eq!(paths[3].surface_key(), "※");
    assert_eq!(paths[3].viterbi_cost, paths[2].viterbi_cost);
    assert_eq!(
        (paths[3].origin, paths[3].priced_by),
        (PathOrigin::Symbol, PathOrigin::Symbol)
    );
    assert_eq!(
        (paths[3].segments[0].left_id, paths[3].segments[0].right_id),
        (2, 3),
        "the entry's ids"
    );
    assert_eq!(placed[0].position, 3);
    downstream(&mut paths, None);
    assert_eq!(index_of(&paths, "※"), Some(3));
}

/// (2) Population indexes 3–5 are a single kanji, hiragana and katakana:
/// the rule steps over them and the symbol goes to index 6.
#[test]
fn the_rule_steps_over_a_single_kanji_and_kana() {
    let dict = symbols_dict(&["※"]);
    let mut paths = words(3);
    paths.push(path("夜", 1300));
    paths.push(path("よみ", 1400)); // the typed kana, all hiragana
    paths.push(path("ヨミ", 1500));
    paths.push(path("四方", 1600));
    let placed = place(&mut paths, &dict);
    assert_eq!(index_of(&paths, "※"), Some(6));
    assert_eq!(paths[6].viterbi_cost, 1500);
    assert_eq!((placed[0].position, placed[0].skipped), (6, 3));
    downstream(&mut paths, None);
    assert_eq!(index_of(&paths, "※"), Some(6));
}

/// (3) A two-path list: the symbol goes last, at index 1's price.
#[test]
fn a_short_list_takes_the_symbol_last() {
    let dict = symbols_dict(&["※"]);
    let mut paths = words(2);
    place(&mut paths, &dict);
    assert_eq!(keys(&paths), ["一二", "三四", "※"]);
    assert_eq!(paths[2].viterbi_cost, 1100);
}

/// (4) Three symbols take consecutive slots in row order.
#[test]
fn several_symbols_keep_row_order() {
    let dict = symbols_dict(&["→", "←", "↑"]);
    let mut paths = words(5);
    let placed = place(&mut paths, &dict);
    assert_eq!(keys(&paths[3..6]), ["→", "←", "↑"]);
    assert!(paths[3..6].iter().all(|p| p.viterbi_cost == 1200));
    assert!(placed.iter().all(|c| c.position == 3));
    downstream(&mut paths, None);
    assert_eq!(keys(&paths[3..6]), ["→", "←", "↑"]);
}

/// (9) An empty list, or no symbol for the reading, is left alone.
#[test]
fn nothing_to_place_is_a_no_op() {
    let dict = symbols_dict(&["※"]);
    let mut empty = Vec::new();
    assert!(place(&mut empty, &dict).is_empty());
    assert!(empty.is_empty());

    let none = symbols_dict(&[]);
    let mut paths = words(4);
    assert!(place(&mut paths, &none).is_empty());
    assert_eq!(paths.len(), 4);

    // A word entry of the reading is not a symbol.
    let word = TrieDictionary::from_entries([(R.to_string(), vec![entry("※", 0)])]);
    let mut paths = words(4);
    assert!(place(&mut paths, &word).is_empty());
    assert_eq!(paths.len(), 4);
}

/// A symbol whose surface is listed already is not added.
#[test]
fn a_listed_surface_is_not_added_again() {
    let dict = symbols_dict(&["※", "＊"]);
    let mut paths = words(4);
    paths[1] = path("※", 1100);
    let placed = place(&mut paths, &dict);
    assert_eq!(placed.len(), 1);
    assert_eq!(paths.iter().filter(|p| p.surface_key_eq("※")).count(), 1);
    assert_eq!(index_of(&paths, "＊"), Some(3));
}

// ---------------------------------------------------------------------------
// The population (paths admission keeps by price) and the exemption
// ---------------------------------------------------------------------------

/// (12) Eight paths; past index 0 only the typed kana (above the limit,
/// exempt) is kept by admission: the population is two paths, the symbol
/// copies the kana's price, sits right after it, and survives admission at
/// index 2.
#[test]
fn the_population_counts_only_what_admission_keeps() {
    let dict = symbols_dict(&["※"]);
    let limit = 1000 + GAP;
    let mut paths = vec![
        path("一二", 1000),
        path("三四", limit + 1000),
        path("五六", limit + 2000),
        identity(limit + 3000),
        path("七八", limit + 4000),
        path("九十", limit + 5000),
        path("百千", limit + 6000),
        path("万億", limit + 7000),
    ];
    let placed = place(&mut paths, &dict);
    assert_eq!(index_of(&paths, "※"), Some(4), "right after the kana");
    assert_eq!(
        placed,
        [SymbolCandidate {
            surface: "※".into(),
            position: 2,
            skipped: 0,
            population: 2,
            price: limit + 3000,
            copied_from_identity: true,
        }]
    );
    downstream(&mut paths, None);
    assert_eq!(keys(&paths), ["一二", R, "※"]);
    assert_eq!(paths[2].admission_gap, None);
}

/// (13) Five paths within the limit and ten beyond: the position is counted
/// in the five, the price is the third's, and the symbol stays at index 3
/// once admission drops the ten.
#[test]
fn paths_beyond_the_limit_do_not_move_the_position() {
    let dict = symbols_dict(&["※"]);
    let mut paths = words(5);
    paths.extend((0..10).map(|i| path(&format!("外{i}"), 1000 + GAP + 1 + i)));
    let placed = place(&mut paths, &dict);
    assert_eq!(
        (placed[0].position, placed[0].population, placed[0].price),
        (3, 5, 1200)
    );
    downstream(&mut paths, None);
    assert_eq!(paths.len(), 6);
    assert_eq!(index_of(&paths, "※"), Some(3));
}

/// (14) Population indexes 3 and 4 are `らーめん` (hiragana with ー) and
/// `ラーメン`: both are stepped over.
#[test]
fn hiragana_with_the_long_vowel_mark_is_stepped_over() {
    let dict = symbols_dict(&["※"]);
    let mut paths = words(3);
    paths.push(path("らーめん", 1300));
    paths.push(path("ラーメン", 1400));
    paths.push(path("四方", 1500));
    place(&mut paths, &dict);
    assert_eq!(index_of(&paths, "※"), Some(5));
}

// ---------------------------------------------------------------------------
// Through the pipeline
// ---------------------------------------------------------------------------

/// Records what the placement reported.
#[derive(Default)]
struct Placed(Vec<SymbolCandidate>);
impl PostprocessObserver for Placed {
    fn symbols_placed(&mut self, placed: &[SymbolCandidate]) {
        self.0 = placed.to_vec();
    }
}

/// Words of [`R`] (two-kanji, so nothing is stepped over) plus symbols.
fn yomi_dict(symbols: &[&str]) -> TrieDictionary {
    let mut entries = vec![
        entry("読み", 1000),
        entry("詠み", 1500),
        entry("黄泉", 2000),
        entry("余味", 2500),
        entry("世見", 3000),
    ];
    entries.extend(symbols.iter().enumerate().map(|(i, s)| symbol(s, i as i16)));
    TrieDictionary::from_entries([(R.to_string(), entries)])
}

fn pipeline(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> (Vec<ScoredPath>, Vec<SymbolCandidate>) {
    let lattice = build_lattice(dict, kana);
    let oversample = crate::converter::population(n, history.is_some());
    let mut raw = viterbi_nbest(&lattice, &DefaultCostFunction::new(conn), oversample);
    let conv = ConversionContext::new(dict, conn).with_history(history, T0);
    let ctx = PostprocessContext {
        lattice: &lattice,
        conv: &conv,
        n,
        max_cost_gap: GAP,
    };
    let mut observer = Placed::default();
    let out = postprocess_observed(&mut raw, &ctx, &mut observer);
    (out, observer.0)
}

/// (5) n = 1 without history: the symbol is placed, then cut with the
/// paths after the one model path; the 1-best is the word.
#[test]
fn at_n_1_the_symbol_is_cut_and_the_one_best_is_the_word() {
    let dict = yomi_dict(&["※"]);
    let (out, placed) = pipeline(&dict, None, None, R, 1);
    assert_eq!(placed.len(), 1, "placed at n = 1 too");
    assert_eq!(out[0].surface_key(), "読み");
    assert_eq!(index_of(&out, "※"), None, "{:?}", keys(&out));
}

/// (6) A learned symbol is #1 after history (at n = 1 as well).
#[test]
fn a_learned_symbol_is_the_one_best() {
    let dict = yomi_dict(&["※"]);
    let h = learned(&[&[(R, "※")]]);
    for n in [1, 20] {
        let (out, _) = pipeline(&dict, None, Some(&h), R, n);
        assert_eq!(out[0].surface_key(), "※", "n = {n}");
        assert_eq!(out[0].origin, PathOrigin::Symbol);
    }
}

/// The candidate list (n = 20, no history): the symbol is at index 3 and
/// the list before it is the list without the symbol.
#[test]
fn the_symbol_joins_the_list_at_its_position() {
    let with = yomi_dict(&["※"]);
    let without = yomi_dict(&[]);
    let (out, placed) = pipeline(&with, None, None, R, 20);
    let (base, _) = pipeline(&without, None, None, R, 20);
    assert_eq!(index_of(&out, "※"), Some(placed[0].position));
    assert_eq!(placed[0].position, 3);
    let mut rest = keys(&out);
    rest.retain(|s| s != "※");
    assert_eq!(rest, keys(&base));
}

/// (7) An unlearned symbol and a path after it whose segment history boosts
/// (unigram): the path passes the symbol, which only moves down (I9″).
#[test]
fn a_boosted_path_passes_an_unlearned_symbol() {
    let dict = symbols_dict(&["※"]);
    let mut paths = words(4);
    paths.push(ScoredPath::new(
        vec![
            RichSegment {
                reading: "よ".into(),
                surface: "夜".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "み".into(),
                surface: "美".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        1400,
        PathOrigin::Viterbi,
    ));
    place(&mut paths, &dict);
    assert_eq!(index_of(&paths, "※"), Some(3));
    let h = learned(&[&[("よ", "夜")]]);
    downstream(&mut paths, Some(&h));
    let word = index_of(&paths, "夜美").unwrap();
    let sym = index_of(&paths, "※").unwrap();
    assert!(!paths[word].is_learned(), "a per-segment boost only");
    assert!(word < sym, "{:?}", keys(&paths));
    assert_eq!(sym, 4);
}

/// (8) A user_dict word with the symbol's surface: the word is listed, the
/// symbol is not added.
#[test]
fn a_user_dict_word_with_the_surface_keeps_the_symbol_out() {
    let system = std::sync::Arc::new(yomi_dict(&["※"]));
    let user = UserDictionary::new();
    user.register(R, "※");
    let dict = CompositeDictionary::new(vec![
        system as std::sync::Arc<dyn Dictionary>,
        std::sync::Arc::new(user),
    ]);
    let (out, placed) = pipeline(&dict, None, None, R, 20);
    assert!(placed.is_empty(), "{placed:?}");
    let listed: Vec<_> = out.iter().filter(|p| p.surface_key_eq("※")).collect();
    assert_eq!(listed.len(), 1);
    assert_ne!(listed[0].origin, PathOrigin::Symbol);
}

/// (11) A reading with no lattice word, only a symbol: the list is the
/// segment paths and the kana, and the symbol goes to the rule's position,
/// not to index 0.
#[test]
fn a_reading_without_a_word_still_places_by_position() {
    let dict = TrieDictionary::from_entries([
        (R.to_string(), vec![symbol("※", 0)]),
        ("よ".to_string(), vec![entry("夜", 1000), entry("代", 1100)]),
        ("み".to_string(), vec![entry("実", 1000), entry("身", 1100)]),
    ]);
    let (out, placed) = pipeline(&dict, None, None, R, 20);
    let at = index_of(&out, "※").unwrap();
    assert!(at > 0, "{:?}", keys(&out));
    assert_eq!(at, placed[0].position, "{:?}", keys(&out));
}

/// (15) The Override stage comes after the placement: a number compound
/// priced below the anchor moves the symbol down by one, and the Katakana
/// fallback lands after the symbol.
#[test]
fn override_paths_only_move_the_symbol_down() {
    const COUNTER: u16 = 7;
    let n = 8usize;
    let text = format!("{n} {n}\n{}", "0\n".repeat(n * n));
    let mut roles = vec![0u8; n];
    roles[COUNTER as usize] = 7; // ROLE_COUNTER
    let conn = ConnectionMatrix::from_text_with_roles(&text, 0, 0, roles).unwrap();
    let dict = TrieDictionary::from_entries([
        ("に".to_string(), vec![entry_with_id("煮", 1000, 1)]),
        ("えん".to_string(), vec![entry_with_id("円", 1000, COUNTER)]),
        ("にえん".to_string(), vec![symbol("¥", 0)]),
    ]);
    let (out, placed) = pipeline(&dict, Some(&conn), None, "にえん", 20);
    let p = placed[0].position;
    let sym = index_of(&out, "¥").unwrap();
    let compound = index_of(&out, "二円").expect("a number compound");
    assert!(compound < sym, "{:?}", keys(&out));
    assert_eq!(sym, p + 1, "{:?}", keys(&out));
    let katakana = index_of(&out, "ニエン").expect("the Katakana fallback");
    assert!(sym < katakana, "{:?}", keys(&out));
}

// ---------------------------------------------------------------------------
// explain
// ---------------------------------------------------------------------------

/// explain records the placement's report and shows the symbol path as
/// placed by Symbol, with no admission verdict and no cost columns.
#[test]
fn explain_records_the_placement() {
    let dict = yomi_dict(&["※"]);
    let conv = ConversionContext::new(&dict, None);
    let result = crate::converter::explain::explain(&conv, R, 20);
    let [placed] = result.symbol_candidates.as_slice() else {
        panic!("{:?}", result.symbol_candidates);
    };
    let (_, pipeline_placed) = pipeline(&dict, None, None, R, 20);
    assert_eq!(result.symbol_candidates, pipeline_placed);
    let p = placed.position;
    assert_eq!((p, placed.skipped), (3, 0));
    let listed = &result.after_rerank;
    assert_eq!(listed[p].surface, "※");
    assert_eq!(listed[p].origin, PathOrigin::Symbol);
    assert_eq!(placed.price, listed[p - 1].viterbi_cost);
    assert_eq!(placed.copied_from_identity, listed[p - 1].is_identity);
    let population = listed
        .iter()
        .filter(|a| a.origin != PathOrigin::Symbol)
        .filter(|a| a.is_identity || a.viterbi_cost <= result.cost_gap_anchor + GAP)
        .count();
    assert_eq!(placed.population, population);

    let path = result
        .paths
        .iter()
        .find(|e| e.surface() == "※")
        .expect("listed");
    assert_eq!(
        (path.origin, path.priced_by),
        (PathOrigin::Symbol, PathOrigin::Symbol)
    );
    assert_eq!(path.admission_gap, None);
    assert_eq!((path.model_cost, path.price_clamp), (None, None));
    assert_eq!(path.length_variance_cost, None);
    assert!(path
        .segments
        .iter()
        .all(|s| s.word_cost.is_none() && s.script_cost.is_none() && s.connection_cost.is_none()));
    assert!(!result.model_paths.iter().any(|m| m.surface == "※"));
}
