#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum LexError {
    #[error("IO error: {msg}")]
    Io { msg: String },
    #[error("invalid data: {msg}")]
    InvalidData { msg: String },
    #[error("internal error: {msg}")]
    Internal { msg: String },
}

#[derive(uniffi::Record)]
pub struct LexDictEntry {
    pub reading: String,
    pub surface: String,
    pub cost: i16,
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
