use lex_core::converter::ConvertedSegment;

use super::types::{AsyncCandidateRequest, CandidateAction, KeyResponse, MarkedText, Submode};
use super::InputSession;

impl InputSession {
    pub(super) fn try_auto_commit(&mut self) -> Option<KeyResponse> {
        if !self.conversion_mode.auto_commit_enabled() {
            return None;
        }
        // Extract data from comp() in a block so the borrow is dropped before
        // we access self.history_records.
        let (committed_reading, committed_surface, seg_pairs, commit_count) = {
            let c = self.comp();
            if c.stability.count < 3 {
                return None;
            }
            let best_path = c.candidates.paths.first()?;
            if best_path.len() < 4 {
                return None;
            }
            if c.candidates.selected != 0 {
                return None;
            }
            if !c.pending.is_empty() {
                return None;
            }

            // Count how many segments to commit (group consecutive ASCII)
            let mut commit_count = 1;
            if best_path[0].surface.is_ascii() {
                while commit_count < best_path.len() - 1
                    && best_path[commit_count].surface.is_ascii()
                {
                    commit_count += 1;
                }
            }

            let segments: Vec<&ConvertedSegment> = best_path[0..commit_count].iter().collect();
            let committed_reading: String = segments.iter().map(|s| s.reading.as_str()).collect();
            let committed_surface: String = segments.iter().map(|s| s.surface.as_str()).collect();

            if !c.kana.starts_with(&committed_reading) {
                return None;
            }

            let seg_pairs: Option<Vec<(String, String)>> = if commit_count > 1 {
                Some(
                    segments
                        .iter()
                        .map(|s| (s.reading.clone(), s.surface.clone()))
                        .collect(),
                )
            } else {
                None
            };

            (
                committed_reading,
                committed_surface,
                seg_pairs,
                commit_count,
            )
        };

        // Record to history (comp() borrow is dropped)
        if committed_surface != committed_reading {
            let pairs = vec![(committed_reading.clone(), committed_surface.clone())];
            self.history_records.push(pairs);
        }
        if let Some(seg_pairs) = seg_pairs {
            self.history_records.push(seg_pairs);
        }

        // Remove committed reading from kana.
        // Safety: starts_with check above guarantees the byte offset is a valid
        // UTF-8 boundary, but we use char-based slicing for extra safety.
        let c = self.comp();
        let skip_chars = committed_reading.chars().count();
        c.kana = c.kana.chars().skip(skip_chars).collect();
        c.stability.reset();

        // Include prefix in the committed text, then clear it
        let prefix_text = std::mem::take(&mut c.prefix.text);
        c.prefix.has_boundary_space = false;
        let mut resp = KeyResponse::consumed();
        resp.commit = Some(format!("{}{}", prefix_text, committed_surface));
        resp.side_effects.save_history = true;

        if self.comp().kana.is_empty() {
            self.comp().candidates.clear();
            resp.candidates = CandidateAction::Hide;
            resp.marked = Some(MarkedText {
                text: String::new(),
                dashed: false,
            });
        } else if self.defer_candidates {
            // Async mode: extract provisional candidates from remaining N-best
            // segments so the candidate panel stays visible (no flicker).
            let c = self.comp();
            let mut provisional: Vec<String> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for path in &c.candidates.paths {
                if path.len() > commit_count {
                    let remaining: String = path[commit_count..]
                        .iter()
                        .map(|s| s.surface.as_str())
                        .collect();
                    if !remaining.is_empty() && seen.insert(remaining.clone()) {
                        provisional.push(remaining);
                    }
                }
            }
            // Always include kana as a fallback candidate
            if seen.insert(c.kana.clone()) {
                provisional.push(c.kana.clone());
            }

            // kana is guaranteed non-empty here (empty case handled above),
            // so provisional always has at least the kana entry.
            debug_assert!(!provisional.is_empty());

            c.candidates.clear();

            // Store provisional candidates in session state so that candidate
            // navigation (Space / Arrow) works during the async phase.
            c.candidates.surfaces.clone_from(&provisional);

            // prefix.text was already consumed into commit via std::mem::take
            // above, so it is empty here — no need to prepend it.
            resp.marked = Some(MarkedText {
                text: provisional[0].clone(),
                dashed: false,
            });
            resp.async_request = Some(AsyncCandidateRequest {
                reading: c.kana.clone(),
                candidate_dispatch: self.conversion_mode.candidate_dispatch(),
            });
            resp.candidates = CandidateAction::Show {
                surfaces: provisional,
                selected: 0,
            };
        } else {
            // Sync mode: re-generate candidates for remaining input
            let c = self.comp();
            let dashed = c.submode == Submode::English;
            let display = c.display_kana();
            resp.marked = Some(MarkedText {
                text: display,
                dashed,
            });
            self.update_candidates();
            let c = self.comp();
            if let Some(best) = c.candidates.surfaces.first() {
                resp.marked = Some(MarkedText {
                    text: format!("{}{}", c.prefix.text, best),
                    dashed,
                });
            }
            if !c.candidates.is_empty() {
                resp.candidates = CandidateAction::Show {
                    surfaces: c.candidates.surfaces.clone(),
                    selected: c.candidates.selected as u32,
                };
            }
        }

        Some(resp)
    }
}
