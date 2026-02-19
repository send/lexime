use std::collections::HashSet;

use tracing::{debug, debug_span};

use crate::converter::{convert_nbest, convert_nbest_with_history};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::settings::settings;
use crate::user_history::UserHistory;

use super::{generate_punctuation_candidates, punctuation_alternatives, CandidateResponse};

/// Generate candidates for normal (non-punctuation) input.
pub(super) fn generate_normal_candidates(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();

    // 1. N-best Viterbi conversion with history-aware reranking.
    //    history_rerank is applied post-Viterbi on N-best paths (not during
    //    lattice search), so it cannot cause fragmentation. Time-decayed
    //    boosts (half-life 168h) prevent stale history from dominating.
    let nbest = settings().candidates.nbest;
    let paths = if let Some(h) = history {
        convert_nbest_with_history(dict, conn, h, reading, nbest)
    } else {
        convert_nbest(dict, conn, reading, nbest)
    };

    let mut nbest_paths = Vec::new();
    for path in &paths {
        let joined: String = path.iter().map(|s| s.surface.as_str()).collect();
        if !joined.is_empty() && seen.insert(joined.clone()) {
            surfaces.push(joined);
        }
        nbest_paths.push(path.clone());
    }

    // 1.5. Inject history-learned surfaces not in N-best.
    if let Some(h) = history {
        let now = crate::user_history::now_epoch();
        for (surface, _boost) in h.learned_surfaces(reading, now) {
            if seen.insert(surface.clone()) {
                surfaces.push(surface);
            }
        }
    }

    // 2. Reading (kana) — position depends on history boost.
    //    If the user previously selected the hiragana form, promote it —
    //    but only above the N-best #1 when the #1 hasn't been explicitly
    //    learned. When the #1 has its own whole-path history boost, kana
    //    goes to position 1 instead so explicit kanji selection is respected.
    let now = crate::user_history::now_epoch();
    let kana_boost = history.map_or(0, |h| h.unigram_boost(reading, reading, now));
    let top_has_boost = if !surfaces.is_empty() && surfaces[0] != reading {
        history.is_some_and(|h| h.unigram_boost(reading, &surfaces[0], now) > 0)
    } else {
        false
    };
    let kana_target = if kana_boost > 0 && !top_has_boost {
        0
    } else {
        1
    };
    let kana_existing_pos = surfaces.iter().position(|s| s == reading);
    if kana_boost > 0 {
        match kana_existing_pos {
            Some(pos) if pos == kana_target => {} // already where we want it
            Some(pos) => {
                surfaces.remove(pos);
                let at = kana_target.min(surfaces.len());
                surfaces.insert(at, reading.to_string());
            }
            None => {
                seen.insert(reading.to_string());
                let at = kana_target.min(surfaces.len());
                surfaces.insert(at, reading.to_string());
            }
        }
    } else if kana_existing_pos.is_none() {
        seen.insert(reading.to_string());
        surfaces.push(reading.to_string());
    }

    // 3. Predictions (ranked by history if available)
    let fetch_limit = if history.is_some() {
        max_results.max(200)
    } else {
        max_results
    };
    let mut ranked = dict.predict_ranked(reading, fetch_limit, 1000);
    if let Some(h) = history {
        ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
            let boost_a = h.unigram_boost(r_a, &e_a.surface, now);
            let boost_b = h.unigram_boost(r_b, &e_b.surface, now);
            boost_b.cmp(&boost_a).then(e_a.cost.cmp(&e_b.cost))
        });
        ranked.truncate(max_results);
    }
    for (_, entry) in &ranked {
        if !entry.surface.is_empty() && seen.insert(entry.surface.clone()) {
            surfaces.push(entry.surface.clone());
        }
    }

    // 4. Dictionary lookup
    let lookup_entries = dict.lookup(reading);
    if let Some(h) = history {
        if !lookup_entries.is_empty() {
            let reordered = h.reorder_candidates(reading, &lookup_entries);
            for entry in &reordered {
                if seen.insert(entry.surface.clone()) {
                    surfaces.push(entry.surface.clone());
                }
            }
        }
    } else {
        for entry in &lookup_entries {
            if seen.insert(entry.surface.clone()) {
                surfaces.push(entry.surface.clone());
            }
        }
    }

    CandidateResponse {
        surfaces,
        paths: nbest_paths,
    }
}

/// Unified candidate generation: handles both punctuation and normal input.
pub fn generate(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let _span = debug_span!("generate_candidates", reading, max_results).entered();
    if reading.is_empty() {
        return CandidateResponse {
            surfaces: Vec::new(),
            paths: Vec::new(),
        };
    }

    let resp = if punctuation_alternatives(reading).is_some() {
        generate_punctuation_candidates(dict, history, reading, max_results)
    } else {
        generate_normal_candidates(dict, conn, history, reading, max_results)
    };
    debug!(
        surface_count = resp.surfaces.len(),
        path_count = resp.paths.len()
    );
    resp
}
