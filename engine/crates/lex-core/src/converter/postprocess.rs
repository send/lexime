use tracing::debug_span;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use super::lattice::Lattice;
use super::reranker;
use super::resegment;
use super::rewriter;
use super::viterbi::{ConvertedSegment, RichSegment, ScoredPath};

/// Shared post-processing pipeline: resegment → rerank → hiragana_rewrite → history_rerank → take(n) → rewrite → group.
pub(super) fn postprocess(
    paths: &mut Vec<ScoredPath>,
    lattice: &Lattice,
    conn: Option<&ConnectionMatrix>,
    dict: Option<&dyn Dictionary>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    let _span = debug_span!("postprocess", n, paths_in = paths.len()).entered();

    // Generate alternative segmentations from the lattice before reranking,
    // so the reranker can compare them on equal footing with Viterbi paths.
    let reseg_paths = resegment::resegment(paths, lattice, conn);
    paths.extend(reseg_paths);

    reranker::rerank(paths, conn, dict);

    // Hiragana variant must run BEFORE history_rerank so that whole-path
    // unigram boosts (×5) can promote a previously-selected hiragana variant.
    let hiragana_rw = rewriter::HiraganaVariantRewriter;
    let partial_rw = rewriter::PartialHiraganaRewriter;
    rewriter::run_rewriters(&[&hiragana_rw, &partial_rw], paths, kana);

    // Remember the pure-Viterbi best surface before history reranking.
    // History boosts per-segment unigrams (e.g. き→機 from past "機械") which can
    // push fragmented single-char paths above the statistically correct compound
    // path (e.g. きがし→気がし). Preserving the Viterbi #1 ensures it is always
    // available as a candidate.
    let viterbi_best_key = if history.is_some() && !paths.is_empty() {
        Some(paths[0].surface_key())
    } else {
        None
    };

    if let Some(h) = history {
        reranker::history_rerank(paths, h);
    }
    let mut top: Vec<ScoredPath> = paths.drain(..n.min(paths.len())).collect();

    // If the Viterbi #1 was pushed out of the top-n by history boosts, pull it
    // back in (after the history-preferred #1, or at 0 if top is empty).
    if let Some(ref best_key) = viterbi_best_key {
        if !top.iter().any(|p| p.surface_key_eq(best_key)) {
            if let Some(pos) = paths.iter().position(|p| p.surface_key_eq(best_key)) {
                let best = paths.remove(pos);
                let insert_at = 1.min(top.len());
                top.insert(insert_at, best);
            }
        }
    }
    // Truncate Viterbi paths to n before rewriters so that rewriter-added
    // candidates (numeric, katakana) are not immediately pruned.
    top.truncate(n);
    let numeric_rw = rewriter::NumericRewriter;
    let katakana_rw = rewriter::KatakanaRewriter;
    let kanji_rw = rewriter::KanjiVariantRewriter { lattice };
    rewriter::run_rewriters(&[&numeric_rw, &katakana_rw, &kanji_rw], &mut top, kana);
    if let Some(c) = conn {
        for path in &mut top {
            group_segments(&mut path.segments, c);
        }
    }
    top.into_iter().map(|p| p.into_segments()).collect()
}

/// Group morpheme-level segments into phrase-level segments (bunsetsu).
///
/// Rules:
/// - **FunctionWord / Suffix**: merge into the preceding group (same as trailing particle).
/// - **Prefix**: start a new group that absorbs the next content word.
/// - **ContentWord**: if a pending prefix exists, merge into it; otherwise start a new group.
/// - Leading function words / suffixes with no preceding group stay standalone.
pub(super) fn group_segments(segments: &mut Vec<RichSegment>, conn: &ConnectionMatrix) {
    if segments.len() <= 1 {
        return;
    }

    let mut grouped: Vec<RichSegment> = Vec::new();
    let mut current: Option<RichSegment> = None;
    let mut pending_prefix = false;

    for seg in segments.drain(..) {
        let role = conn.role(seg.left_id);
        let is_fw = conn.is_function_word(seg.left_id);
        let attach_to_prev = is_fw || role == 2; // FunctionWord or Suffix

        if attach_to_prev {
            // Merge into current group if one exists
            if let Some(cur) = current.as_mut() {
                cur.reading.push_str(&seg.reading);
                cur.surface.push_str(&seg.surface);
                cur.right_id = seg.right_id;
            } else {
                // No preceding group — standalone
                grouped.push(seg);
            }
        } else if role == 3 {
            // Prefix: flush current group, start new one that will absorb next CW
            if let Some(cur) = current.take() {
                grouped.push(cur);
            }
            current = Some(seg);
            pending_prefix = true;
        } else {
            // ContentWord
            if pending_prefix {
                // Merge CW into the pending prefix group
                if let Some(cur) = current.as_mut() {
                    cur.reading.push_str(&seg.reading);
                    cur.surface.push_str(&seg.surface);
                    cur.right_id = seg.right_id;
                }
                pending_prefix = false;
            } else {
                // New group
                if let Some(cur) = current.take() {
                    grouped.push(cur);
                }
                current = Some(seg);
            }
        }
    }

    if let Some(cur) = current {
        grouped.push(cur);
    }

    *segments = grouped;
}
