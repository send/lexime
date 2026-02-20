use crate::session::{CandidateAction, KeyResponse};

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
    ClearMarkedText,
    ShowCandidates {
        surfaces: Vec<String>,
        selected: u32,
    },
    HideCandidates,
    SwitchToAbc,
    SchedulePoll,
}

#[derive(uniffi::Enum)]
pub enum LexRomajiLookup {
    None,
    Prefix,
    Exact { kana: String },
    ExactAndPrefix { kana: String },
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
