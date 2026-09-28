mod hiragana_variant;
mod kanji_variant;
mod katakana;
mod numeric;
mod partial_hiragana;
mod priced_offers;
mod run_rewriters;

use crate::converter::cost::score_path;
use crate::converter::reranker::FeaturePricer;
use crate::converter::rewriter::OFFER_CAP;
use crate::converter::viterbi::ScoredPath;

/// The feature pricer the offer tests run under: no connection matrix, no
/// dictionary.
fn pricer() -> FeaturePricer<'static> {
    FeaturePricer::new(None, None)
}

/// An offer's price: what rerank would charge its segments, within
/// [source, source + OFFER_CAP].
fn offer_price(offer: &ScoredPath, src: &ScoredPath) -> i64 {
    let model = score_path(&offer.segments, None) + pricer().adjustment(offer);
    let base = src.pre_history_cost();
    model.clamp(base, base + OFFER_CAP)
}
