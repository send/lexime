use crate::session::{CandidateAction, KeyEvent, KeyResponse};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum LexError {
    #[error("IO error: {msg}")]
    Io { msg: String },
    #[error("invalid data: {msg}")]
    InvalidData { msg: String },
    #[error("internal error: {msg}")]
    Internal { msg: String },
}

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

// ---------------------------------------------------------------------------
// Records (value types, copied across FFI boundary)
// ---------------------------------------------------------------------------

#[derive(Clone, uniffi::Record)]
pub struct LexSegment {
    pub reading: String,
    pub surface: String,
}

#[derive(uniffi::Record)]
pub struct LexDictEntry {
    pub reading: String,
    pub surface: String,
    pub cost: i16,
}

#[derive(Clone, uniffi::Record)]
pub struct LexCandidateResult {
    pub surfaces: Vec<String>,
    pub paths: Vec<Vec<LexSegment>>,
}

#[derive(uniffi::Record)]
pub struct LexUserWord {
    pub reading: String,
    pub surface: String,
}

#[derive(uniffi::Record)]
pub struct LexSnippetEntry {
    pub key: String,
    pub body: String,
}

#[derive(uniffi::Record)]
pub struct LexRomajiConvert {
    pub composed_kana: String,
    pub pending_romaji: String,
}

/// Event-driven response from handle_key / commit / poll.
#[derive(uniffi::Record)]
pub struct LexKeyResponse {
    pub consumed: bool,
    pub events: Vec<LexEvent>,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, uniffi::Enum)]
pub enum LexEvent {
    Commit {
        text: String,
    },
    SetMarkedText {
        text: String,
    },
    ShowCandidates {
        surfaces: Vec<String>,
        selected: u32,
    },
    HideCandidates,
    SwitchToAbc,
    SchedulePoll,
}

#[derive(uniffi::Enum)]
pub enum LexConversionMode {
    Standard,
    Predictive,
}

#[derive(uniffi::Enum)]
pub enum LexRomajiLookup {
    None,
    Prefix,
    Exact { kana: String },
    ExactAndPrefix { kana: String },
}

/// Platform-independent key event for FFI.
#[derive(uniffi::Enum)]
pub enum LexKeyEvent {
    Text { text: String, shift: bool },
    Remapped { text: String, shift: bool },
    Enter,
    Space,
    Backspace,
    Escape,
    Tab,
    ArrowDown,
    ArrowUp,
    SwitchToDirectInput,
    SwitchToJapanese,
    ForwardDelete,
    ModifiedKey,
    SnippetTrigger,
}

/// Trigger key descriptor for snippet expansion (character-based matching).
#[derive(uniffi::Record)]
pub struct LexTriggerKey {
    /// The character to match (e.g. ";"). Named `char_` to avoid conflicts in generated bindings.
    pub char_: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
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

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

pub(super) fn convert_to_events(resp: KeyResponse, has_pending_work: bool) -> LexKeyResponse {
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

    // 5. Schedule poll
    if has_pending_work {
        events.push(LexEvent::SchedulePoll);
    }

    LexKeyResponse {
        consumed: resp.consumed,
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
        let result = convert_to_events(resp, false);
        assert!(!result.consumed);
        assert!(result.events.is_empty());
    }

    #[test]
    fn test_convert_commit_event() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.commit = Some("テスト".to_string());
        let result = convert_to_events(resp, false);
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
        let result = convert_to_events(resp, false);
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
        let result = convert_to_events(resp, false);
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
        let result = convert_to_events(resp, false);
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
        let result = convert_to_events(resp, false);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::HideCandidates));
    }

    #[test]
    fn test_convert_schedule_poll() {
        let resp = empty_response();
        let result = convert_to_events(resp, true);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(&result.events[0], LexEvent::SchedulePoll));
    }

    #[test]
    fn test_convert_switch_to_abc() {
        let mut resp = empty_response();
        resp.consumed = true;
        resp.side_effects.switch_to_abc = true;
        let result = convert_to_events(resp, false);
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
        let result = convert_to_events(resp, true);
        assert!(result.consumed);
        // commit + marked + candidates + poll = 4
        assert_eq!(result.events.len(), 4);
        assert!(matches!(&result.events[0], LexEvent::Commit { .. }));
        assert!(matches!(&result.events[1], LexEvent::SetMarkedText { .. }));
        assert!(matches!(&result.events[2], LexEvent::ShowCandidates { .. }));
        assert!(matches!(&result.events[3], LexEvent::SchedulePoll));
    }
}
