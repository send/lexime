use tracing::debug_span;

use crate::dict::connection::ConnectionMatrix;
use crate::user_history::UserHistory;

use super::reranker;
use super::rewriter;
use super::viterbi::{ConvertedSegment, RichSegment, ScoredPath};

/// Shared post-processing pipeline: rerank → history_rerank → take(n) → rewrite → group.
pub(super) fn postprocess(
    paths: &mut Vec<ScoredPath>,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    let _span = debug_span!("postprocess", n, paths_in = paths.len()).entered();
    reranker::rerank(paths, conn);
    if let Some(h) = history {
        reranker::history_rerank(paths, h);
    }
    let mut top: Vec<ScoredPath> = paths.drain(..n.min(paths.len())).collect();
    let katakana_rw = rewriter::KatakanaRewriter;
    let rewriters: Vec<&dyn rewriter::Rewriter> = vec![&katakana_rw];
    rewriter::run_rewriters(&rewriters, &mut top, kana);
    // Rewriters may append extra candidates (e.g. katakana fallback);
    // truncate back to the requested n to honour the caller's limit.
    top.truncate(n);
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
