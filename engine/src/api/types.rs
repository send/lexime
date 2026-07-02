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
    /// Session epoch this response reflects (monotonic, stamped under the
    /// session lock). The Rust-side epoch gate guarantees session *state*
    /// consistency, but async responses are re-queued onto the platform main
    /// thread while synchronous key responses are applied inline — so an
    /// already-accepted async response can reach the UI *after* a newer key
    /// response. The frontend must track the highest epoch it has applied
    /// and silently drop any async response with a lower epoch.
    #[uniffi(default = 0)]
    pub epoch: u64,
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
