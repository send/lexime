use crate::session::{CandidateAction, KeyEvent, KeyResponse};

use super::types::{LexError, LexEvent, LexKeyEvent, LexKeyResponse};

impl From<std::io::Error> for LexError {
    fn from(e: std::io::Error) -> Self {
        Self::Io { msg: e.to_string() }
    }
}

impl From<crate::dict::DictError> for LexError {
    fn from(e: crate::dict::DictError) -> Self {
        match e {
            crate::dict::DictError::Io(io_err) => Self::Io {
                msg: io_err.to_string(),
            },
            other => Self::InvalidData {
                msg: other.to_string(),
            },
        }
    }
}

impl From<LexKeyEvent> for KeyEvent {
    fn from(e: LexKeyEvent) -> Self {
        match e {
            LexKeyEvent::Text { text, shift } => KeyEvent::Text { text, shift },
            LexKeyEvent::Remapped { text, shift } => KeyEvent::Remapped { text, shift },
            LexKeyEvent::Enter => KeyEvent::Enter,
            LexKeyEvent::Space => KeyEvent::Space,
            LexKeyEvent::Backspace => KeyEvent::Backspace,
            LexKeyEvent::Escape => KeyEvent::Escape,
            LexKeyEvent::Tab => KeyEvent::Tab,
            LexKeyEvent::ArrowDown => KeyEvent::ArrowDown,
            LexKeyEvent::ArrowUp => KeyEvent::ArrowUp,
            LexKeyEvent::SwitchToDirectInput => KeyEvent::SwitchToDirectInput,
            LexKeyEvent::SwitchToJapanese => KeyEvent::SwitchToJapanese,
            LexKeyEvent::ForwardDelete => KeyEvent::ForwardDelete,
            LexKeyEvent::ModifiedKey => KeyEvent::ModifiedKey,
            LexKeyEvent::SnippetTrigger => KeyEvent::SnippetTrigger,
        }
    }
}

/// Convert an internal `KeyResponse` into the FFI event stream.
///
/// `epoch` is the session epoch this response reflects. It must be read via
/// `InputSession::epoch()` while still holding the session lock that produced
/// `resp`, so the frontend can order responses on its UI thread.
pub(super) fn convert_to_events(resp: KeyResponse, epoch: u64) -> LexKeyResponse {
    let mut events = Vec::new();

    // 1. Commit
    if let Some(text) = resp.commit {
        events.push(LexEvent::Commit { text });
    }

    // 2. Marked text
    if let Some(m) = resp.marked {
        events.push(LexEvent::SetMarkedText { text: m.text });
    }

    // 3. Candidates
    match resp.candidates {
        CandidateAction::Show { surfaces, selected } => {
            events.push(LexEvent::ShowCandidates { surfaces, selected });
        }
        CandidateAction::Hide => events.push(LexEvent::HideCandidates),
        CandidateAction::Keep => {}
    }

    // 4. Side effects
    if resp.side_effects.switch_to_abc {
        events.push(LexEvent::SwitchToAbc);
    }

    LexKeyResponse {
        consumed: resp.consumed,
        epoch,
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{CandidateAction, KeyResponse, MarkedText, SideEffects};

    fn empty_response() -> KeyResponse {
        KeyResponse {
            consumed: false,
            commit: None,
            marked: None,
            candidates: CandidateAction::Keep,
            async_request: None,
            side_effects: SideEffects::default(),
        }
    }

    #[test]
    fn test_convert_empty_response() {
        let resp = empty_response();
        let result = convert_to_events(resp, 0);
        assert!(!result.consumed);
        assert!(result.events.is_empty());
    }

    #[test]
    fn test_convert_stamps_epoch() {
        let resp = empty_response();
        let result = convert_to_events(resp, 42);
        assert_eq!(result.epoch, 42);
    }

    #[test]
    fn test_convert_commit_event() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.commit = Some("テスト".to_string());
        let result = convert_to_events(resp, 0);
        assert!(result.consumed);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::Commit { text } if text == "テスト"));
    }

    #[test]
    fn test_convert_marked_text_event() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.marked = Some(MarkedText {
            text: "かな".to_string(),
        });
        let result = convert_to_events(resp, 0);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::SetMarkedText { text } if text == "かな"));
    }

    #[test]
    fn test_convert_clear_marked_text() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.marked = Some(MarkedText {
            text: String::new(),
        });
        let result = convert_to_events(resp, 0);
        // Empty marked text becomes SetMarkedText with empty string
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::SetMarkedText { text } if text.is_empty()));
    }

    #[test]
    fn test_convert_candidates_show() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.candidates = CandidateAction::Show {
            surfaces: vec!["候補1".to_string(), "候補2".to_string()],
            selected: 0,
        };
        let result = convert_to_events(resp, 0);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(
            &result.events[0],
            LexEvent::ShowCandidates { surfaces, selected }
                if surfaces.len() == 2 && *selected == 0
        ));
    }

    #[test]
    fn test_convert_candidates_hide() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.candidates = CandidateAction::Hide;
        let result = convert_to_events(resp, 0);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::HideCandidates));
    }

    #[test]
    fn test_convert_switch_to_abc() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.side_effects.switch_to_abc = true;
        let result = convert_to_events(resp, 0);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::SwitchToAbc));
    }

    #[test]
    fn test_convert_multiple_events() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.commit = Some("確定".to_string());
        resp.marked = Some(MarkedText {
            text: "次の入力".to_string(),
        });
        resp.candidates = CandidateAction::Show {
            surfaces: vec!["a".to_string()],
            selected: 0,
        };
        let result = convert_to_events(resp, 0);
        assert!(result.consumed);
        // commit + marked + candidates = 3
        assert_eq!(result.events.len(), 3);
        assert!(matches!(&result.events[0], LexEvent::Commit { .. }));
        assert!(matches!(&result.events[1], LexEvent::SetMarkedText { .. }));
        assert!(matches!(&result.events[2], LexEvent::ShowCandidates { .. }));
    }

    #[test]
    fn commit_precedes_marked_text() {
        // Order is load-bearing, not cosmetic. Applying `Commit` calls
        // `insertText`, which ends the host's marked session and clears
        // `SessionCoordinator.currentDisplay`; `SetMarkedText` then re-opens it.
        // Emitted the other way round, a response that commits a stable prefix
        // and keeps composing the remainder would leave the host with no marked
        // text while the session is still composing — Chromium/Electron then
        // report `isComposing == false` and the next confirming key reaches the
        // web page as well as the IME (PR #293).
        //
        // The session-side proptest (`lex-session` `proptest_fsm::HostMarked`)
        // models the host by assuming this order; it cannot call this function,
        // since `lex_engine` depends on `lex-session` and not the reverse. This
        // test is the other half of that contract.
        let mut resp = empty_response();
        resp.commit = Some("今日".to_string());
        resp.marked = Some(MarkedText {
            text: "は".to_string(),
        });
        let result = convert_to_events(resp, 0);
        let commit_at = result
            .events
            .iter()
            .position(|e| matches!(e, LexEvent::Commit { .. }))
            .expect("commit event");
        let marked_at = result
            .events
            .iter()
            .position(|e| matches!(e, LexEvent::SetMarkedText { .. }))
            .expect("marked event");
        assert!(
            commit_at < marked_at,
            "Commit must precede SetMarkedText so the marked text survives the commit"
        );
    }
}
