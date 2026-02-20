use std::sync::Arc;

use lex_core::candidates::CandidateResponse;
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::Dictionary;

use super::type_string;
use crate::types::MAX_CANDIDATES;
use crate::InputSession;

/// Headless IME simulator for integration tests.
///
/// Wraps `InputSession` with `defer_candidates = true` (matching the real async path)
/// and provides helpers that drive the full type → generate → receive → commit cycle.
pub(super) struct HeadlessIME {
    pub session: InputSession,
    dict: Arc<dyn Dictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
}

impl HeadlessIME {
    pub fn new(dict: Arc<dyn Dictionary>, conn: Option<Arc<ConnectionMatrix>>) -> Self {
        let mut session = InputSession::new(dict.clone(), conn.clone(), None);
        session.set_defer_candidates(true);
        Self {
            session,
            dict,
            conn,
        }
    }

    /// Type romaji, resolve async candidates, press Enter, return committed text.
    pub fn convert(&mut self, romaji: &str) -> String {
        let mut committed = String::new();

        // 1. Type romaji one character at a time, collecting any auto-commits
        for ch in romaji.chars() {
            let resp = self.session.handle_key(0, &ch.to_string(), 0);
            if let Some(ref text) = resp.commit {
                committed.push_str(text);
            }
            // Resolve async request if present
            if resp.async_request.is_some() {
                if let Some(auto_text) = self.resolve_async() {
                    committed.push_str(&auto_text);
                }
            }
        }

        // 2. Press Enter to commit remaining composition
        if self.session.is_composing() {
            let resp = self.session.handle_key(super::key::ENTER, "", 0);
            if let Some(ref text) = resp.commit {
                committed.push_str(text);
            }
        }

        committed
    }

    /// Type romaji, resolve async candidates, return the composing display (no commit).
    pub fn composing_display(&mut self, romaji: &str) -> String {
        let responses = type_string(&mut self.session, romaji);
        // Resolve the last async request
        if let Some(resp) = responses.last() {
            if resp.async_request.is_some() {
                self.resolve_async();
            }
        }
        self.session.composed_string()
    }

    /// Reset session to idle (simulates commitComposition).
    pub fn reset(&mut self) {
        if self.session.is_composing() {
            self.session.commit();
        }
    }

    /// Complete one async candidate cycle: generate candidates for current reading,
    /// then feed them back. Returns committed text from auto-commit chain, if any.
    fn resolve_async(&mut self) -> Option<String> {
        if !self.session.is_composing() {
            return None;
        }
        let reading = self.session.comp().kana.clone();
        if reading.is_empty() {
            return None;
        }

        let mode = self.session.config.conversion_mode;
        let cand: CandidateResponse = mode.generate_candidates(
            &*self.dict,
            self.conn.as_deref(),
            None,
            &reading,
            MAX_CANDIDATES,
        );
        let resp = self
            .session
            .receive_candidates(&reading, cand.surfaces, cand.paths);

        match resp {
            Some(r) => {
                let mut committed = String::new();
                if let Some(ref text) = r.commit {
                    committed.push_str(text);
                }
                // Auto-commit may leave a residual composition needing its own async resolve
                if r.async_request.is_some() {
                    if let Some(more) = self.resolve_async() {
                        committed.push_str(&more);
                    }
                }
                if committed.is_empty() {
                    None
                } else {
                    Some(committed)
                }
            }
            None => None,
        }
    }
}

#[test]
fn test_headless_convert_single_word() {
    let dict = super::make_test_dict();
    let mut ime = HeadlessIME::new(dict, None);
    let result = ime.convert("kyou");
    assert_eq!(result, "今日");
}

#[test]
fn test_headless_composing_display() {
    let dict = super::make_test_dict();
    let mut ime = HeadlessIME::new(dict, None);
    let display = ime.composing_display("kyou");
    assert_eq!(display, "きょう");
}

#[test]
fn test_headless_reset() {
    let dict = super::make_test_dict();
    let mut ime = HeadlessIME::new(dict, None);
    ime.composing_display("kyou");
    assert!(ime.session.is_composing());
    ime.reset();
    assert!(!ime.session.is_composing());
}
