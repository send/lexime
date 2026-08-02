import Foundation

/// A non-recoverable or degraded condition detected during engine startup.
/// Recorded for visibility only — fallback behavior is unchanged.
enum EngineInitFailure {
    /// System dictionary failed to load. Fatal: the engine cannot start.
    case dictionary(detail: String)
    /// User dictionary file could not be opened (entries unavailable).
    /// Environmental read failures only (e.g. permissions) — corruption no
    /// longer throws (it quarantines; see `.userDictionaryDataLoss`).
    case userDictionary(detail: String)
    /// User dictionary recovery quarantined a corrupt file: registration keeps
    /// working (empty dictionary), but the previously registered words were
    /// lost (bytes preserved in `.corrupt-*` when the rename succeeded).
    case userDictionaryDataLoss(detail: String)
    /// Composite (system + user) dictionary creation failed;
    /// user dictionary entries are not reflected in conversion.
    case compositeDictionary(detail: String)
    /// User history could not be opened (learning disabled). Environmental
    /// read failures only (e.g. permissions) — corruption no longer throws.
    case history(detail: String)
    /// User history recovery quarantined corrupt data: learning is running,
    /// but some past learning was lost (bytes preserved in `.corrupt-*`).
    case historyDataLoss(detail: String)
    /// Custom settings.toml exists but failed to parse (defaults in effect).
    case customSettings(detail: String)
}
