//! Offers priced under a real-shaped connection matrix: non-zero,
//! asymmetric costs (`cost(a, b) != cost(b, a)`), left and right ids that
//! differ, and roles. With `conn: None` every connection is 0 and every
//! swap's neighbours cancel out, so a pricing error passes unseen.

use crate::converter::cost::score_path;
use crate::converter::lattice::Lattice;
use crate::converter::reranker::FeaturePricer;
use crate::converter::rewriter::{
    KanjiVariantRewriter, PartialHiraganaRewriter, Rewriter, OFFER_CAP,
};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};
use crate::dict::connection::ConnectionMatrix;

// POS ids. 0 is BOS/EOS.
const NOUN: u16 = 1; // 方
const PARTICLE_L: u16 = 2; // が / から (left id)
const VERB_L: u16 = 3; // あった / して (left id)
const KANA_NOUN: u16 = 4; // ほう
const NOUN_ALT: u16 = 5; // 砲
const VERB_R: u16 = 6; // あった / して (right id)
const PARTICLE_R: u16 = 7; // が / から (right id)
const IDS: u16 = 8;

/// Connection costs chosen so the favourite for ほう between あった and が
/// depends on both neighbours: the left one prefers 砲 by 500, the right
/// one prefers 方 by 1000, so 方 wins by 500 — and reading the wrong id on
/// either side, or dropping the right side, makes 砲 win.
fn conn() -> ConnectionMatrix {
    let mut costs = vec![0i16; (IDS as usize) * (IDS as usize)];
    let mut set = |l: u16, r: u16, c: i16| costs[(l as usize) * (IDS as usize) + r as usize] = c;
    set(VERB_R, NOUN, 500);
    set(VERB_R, NOUN_ALT, 0);
    set(NOUN, PARTICLE_L, 0);
    set(NOUN_ALT, PARTICLE_L, 1000);
    // What a swap would read with the wrong id on either side.
    set(VERB_L, NOUN, 2000);
    set(NOUN, PARTICLE_R, 1500);
    // Asymmetric, and non-zero around the kana node and the sentence ends.
    set(PARTICLE_L, NOUN, 300);
    set(VERB_R, KANA_NOUN, 200);
    set(KANA_NOUN, PARTICLE_L, 100);
    set(0, VERB_L, 50);
    set(PARTICLE_R, 0, 80);
    let mut roles = vec![0u8; IDS as usize];
    roles[PARTICLE_L as usize] = 1;
    roles[PARTICLE_R as usize] = 1;
    ConnectionMatrix::new_owned(IDS, PARTICLE_L, PARTICLE_L, roles, costs)
}

fn seg(reading: &str, surface: &str, left_id: u16, right_id: u16, word_cost: i16) -> RichSegment {
    RichSegment {
        reading: reading.into(),
        surface: surface.into(),
        left_id,
        right_id,
        word_cost,
    }
}

/// Price a path the way rerank does: Viterbi plus the features.
fn model_price(
    segments: Vec<RichSegment>,
    conn: &ConnectionMatrix,
    origin: PathOrigin,
) -> ScoredPath {
    let mut p = ScoredPath::new(segments, 0, origin);
    p.viterbi_cost =
        score_path(&p.segments, Some(conn)) + FeaturePricer::new(Some(conn), None).adjustment(&p);
    p
}

/// The offer oracle: the variant's model price within [src, src + OFFER_CAP].
fn oracle(v: &ScoredPath, src: &ScoredPath, conn: &ConnectionMatrix) -> (i64, i64) {
    let model =
        score_path(&v.segments, Some(conn)) + FeaturePricer::new(Some(conn), None).adjustment(v);
    let base = src.pre_history_cost();
    (model, model.clamp(base, base + OFFER_CAP))
}

fn kanji_offers(
    lattice: &Lattice,
    conn: &ConnectionMatrix,
    src: &ScoredPath,
    reading: &str,
) -> Vec<ScoredPath> {
    let pricer = FeaturePricer::new(Some(conn), None);
    KanjiVariantRewriter {
        lattice,
        conn: Some(conn),
        pricer: &pricer,
    }
    .generate(std::slice::from_ref(src), reading)
}

fn partial_offers(
    lattice: &Lattice,
    conn: &ConnectionMatrix,
    src: &ScoredPath,
    reading: &str,
) -> Vec<ScoredPath> {
    let pricer = FeaturePricer::new(Some(conn), None);
    PartialHiraganaRewriter {
        lattice,
        conn: Some(conn),
        pricer: &pricer,
    }
    .generate(std::slice::from_ref(src), reading)
}

fn atta() -> RichSegment {
    seg("あった", "あった", VERB_L, VERB_R, 0)
}

fn ga() -> RichSegment {
    seg("が", "が", PARTICLE_L, PARTICLE_R, 0)
}

#[test]
fn kanji_offer_is_the_neighbour_aware_favourite_priced_inside_the_band() {
    let c = conn();
    let lattice = Lattice::from_test_nodes(
        "あったほうが",
        &[
            (3, 5, "ほう", "方", 1000, NOUN, NOUN),
            (3, 5, "ほう", "砲", 1000, NOUN_ALT, NOUN_ALT),
        ],
    );
    let src = model_price(
        vec![atta(), seg("ほう", "ほう", KANA_NOUN, KANA_NOUN, 0), ga()],
        &c,
        PathOrigin::Viterbi,
    );
    let offers = kanji_offers(&lattice, &c, &src, "あったほうが");
    assert_eq!(offers.len(), 1);
    let v = &offers[0];
    assert_eq!(
        v.surface_key(),
        "あった方が",
        "both neighbours decide the favourite"
    );
    let (model, price) = oracle(v, &src, &c);
    assert_eq!(v.viterbi_cost, price);
    // Neither clamp bound binds: the price is the model's own.
    let base = src.pre_history_cost();
    assert!(
        base < model && model < base + OFFER_CAP,
        "{base} < {model} < {}",
        base + OFFER_CAP
    );
}

#[test]
fn partial_offer_is_priced_inside_the_band() {
    let c = conn();
    let lattice = Lattice::from_test_nodes(
        "あったほうが",
        &[(3, 5, "ほう", "ほう", 2000, KANA_NOUN, KANA_NOUN)],
    );
    let src = model_price(
        vec![atta(), seg("ほう", "方", NOUN, NOUN, 1000), ga()],
        &c,
        PathOrigin::Viterbi,
    );
    let offers = partial_offers(&lattice, &c, &src, "あったほうが");
    let v = offers
        .iter()
        .find(|p| p.surface_key() == "あったほうが")
        .unwrap();
    let (model, price) = oracle(v, &src, &c);
    assert_eq!(v.viterbi_cost, price);
    let base = src.pre_history_cost();
    assert!(
        base < model && model < base + OFFER_CAP,
        "{base} < {model} < {}",
        base + OFFER_CAP
    );
    assert_eq!(
        v.segments[1].left_id, KANA_NOUN,
        "the real kana node, not the relabelled kanji"
    );
}

#[test]
fn kanji_offer_keeps_the_word_class() {
    // から is a particle: 殻 (a noun) is not a spelling of it.
    let c = conn();
    let lattice = Lattice::from_test_nodes("してからに", &[(2, 4, "から", "殻", 0, NOUN, NOUN)]);
    let src = model_price(
        vec![
            seg("して", "して", VERB_L, VERB_R, 0),
            seg("から", "から", PARTICLE_L, PARTICLE_R, 0),
            seg("に", "に", PARTICLE_L, PARTICLE_R, 0),
        ],
        &c,
        PathOrigin::Viterbi,
    );
    assert!(kanji_offers(&lattice, &c, &src, "してからに").is_empty());
}

#[test]
fn partial_offer_takes_a_kana_node_of_the_same_class_or_falls_back() {
    let c = conn();
    let src = model_price(
        vec![atta(), seg("ほう", "方", NOUN, NOUN, 1000), ga()],
        &c,
        PathOrigin::Viterbi,
    );
    // A cheaper particle-class ほう loses to the noun-class one.
    let both = Lattice::from_test_nodes(
        "あったほうが",
        &[
            (3, 5, "ほう", "ほう", 0, PARTICLE_L, PARTICLE_R),
            (3, 5, "ほう", "ほう", 2000, KANA_NOUN, KANA_NOUN),
        ],
    );
    let offers = partial_offers(&both, &c, &src, "あったほうが");
    let v = offers
        .iter()
        .find(|p| p.surface_key() == "あったほうが")
        .unwrap();
    assert_eq!(v.segments[1].left_id, KANA_NOUN);
    // Only a particle-class ほう: the kanji node's ids under its reading.
    let other_class = Lattice::from_test_nodes(
        "あったほうが",
        &[(3, 5, "ほう", "ほう", 0, PARTICLE_L, PARTICLE_R)],
    );
    let offers = partial_offers(&other_class, &c, &src, "あったほうが");
    let v = offers
        .iter()
        .find(|p| p.surface_key() == "あったほうが")
        .unwrap();
    assert_eq!(
        (v.segments[1].left_id, v.segments[1].right_id),
        (NOUN, NOUN)
    );
    assert_eq!(v.viterbi_cost, src.pre_history_cost() + OFFER_CAP);
}

#[test]
fn partial_offer_uses_a_kana_node_with_a_long_vowel_mark() {
    // ー is part of a hiragana reading (じーさん), so the real kana node is
    // used rather than the kanji node relabelled.
    let c = conn();
    let lattice = Lattice::from_test_nodes(
        "じーさんが",
        &[(0, 4, "じーさん", "じーさん", 2000, KANA_NOUN, KANA_NOUN)],
    );
    let src = model_price(
        vec![seg("じーさん", "爺さん", NOUN, NOUN, 1000), ga()],
        &c,
        PathOrigin::Viterbi,
    );
    let offers = partial_offers(&lattice, &c, &src, "じーさんが");
    let v = offers
        .iter()
        .find(|p| p.surface_key() == "じーさんが")
        .unwrap();
    assert_eq!(v.segments[0].left_id, KANA_NOUN);
}
