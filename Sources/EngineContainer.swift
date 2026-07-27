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

final class EngineContainer {
    let engine: LexEngine?
    let dictionary: LexDictionary?
    let history: LexUserHistory?
    let userDict: LexUserDictionary?

    /// Initialization failures recorded for user visibility (menu / alert).
    private(set) var initFailures: [EngineInitFailure]

    /// True when any component failed to initialize and the IME is running
    /// in a degraded state (including the fatal engine-nil case).
    var isDegraded: Bool { !initFailures.isEmpty }

    /// Detail message of the fatal dictionary failure, if the engine is unavailable.
    var fatalFailureDetail: String? {
        guard engine == nil else { return nil }
        for case let .dictionary(detail) in initFailures { return detail }
        return nil
    }

    /// Record a failure detected outside `load` (e.g. settings parse errors
    /// caught during bootstrap).
    func recordFailure(_ failure: EngineInitFailure) {
        initFailures.append(failure)
    }

    init(
        engine: LexEngine?,
        dictionary: LexDictionary?,
        history: LexUserHistory?,
        userDict: LexUserDictionary?,
        initFailures: [EngineInitFailure] = []
    ) {
        self.engine = engine
        self.dictionary = dictionary
        self.history = history
        self.userDict = userDict
        self.initFailures = initFailures
    }

    static func load(resourcePath: String, userDictPath: String, historyPath: String) -> EngineContainer {
        var failures: [EngineInitFailure] = []
        let dictPath = (resourcePath as NSString).appendingPathComponent("lexime.dict")
        var dict: LexDictionary?
        do {
            let d = try LexDictionary.open(path: dictPath)
            NSLog("Lexime: Dictionary loaded from %@", dictPath)
            let entries = d.lookup(reading: "かんじ")
            NSLog("Lexime: Sample lookup 'かんじ' → %ld candidates", entries.count)
            dict = d
        } catch {
            NSLog("Lexime: Failed to load dictionary at %@: %@", dictPath, "\(error)")
            failures.append(.dictionary(detail: "\(error)"))
            dict = nil
        }

        let connPath = (resourcePath as NSString).appendingPathComponent("lexime.conn")
        let conn: LexConnection?
        do {
            let c = try LexConnection.open(path: connPath)
            NSLog("Lexime: Connection matrix loaded from %@", connPath)
            conn = c
        } catch {
            NSLog("Lexime: Connection matrix not found at %@ (using unigram fallback)", connPath)
            conn = nil
        }

        let userDict: LexUserDictionary?
        do {
            let ud = try LexUserDictionary.open(path: userDictPath)
            NSLog("Lexime: User dictionary loaded from %@", userDictPath)
            // Recovery report: quarantine = user-visible data loss
            // (registration keeps working with an empty dict; the corrupt
            // bytes are preserved). Recorded here on the SUCCESS path, NOT in
            // `catch`: corruption no longer throws — only environmental read
            // failures do (which land in `.userDictionary` below).
            let report = ud.openReport()
            if report.dataLossSuspected {
                var detail = "user dictionary corrupt; registered words lost"
                if !report.quarantinedPaths.isEmpty {
                    detail += ", quarantined: \(report.quarantinedPaths.joined(separator: ", "))"
                }
                NSLog("Lexime: %@", detail)
                failures.append(.userDictionaryDataLoss(detail: detail))
            }
            userDict = ud
        } catch {
            NSLog("Lexime: Failed to open user dictionary at %@: %@", userDictPath, "\(error)")
            failures.append(.userDictionary(detail: "\(error)"))
            userDict = nil
        }

        if userDict != nil, dict != nil {
            do {
                let composite = try LexDictionary.openWithUserDict(
                    path: dictPath, userDict: userDict)
                NSLog("Lexime: Composite dictionary created (system + user)")
                dict = composite
            } catch {
                NSLog("Lexime: Failed to create composite dictionary: %@", "\(error)")
                failures.append(.compositeDictionary(detail: "\(error)"))
            }
        }

        let history: LexUserHistory?
        do {
            let h = try LexUserHistory.open(path: historyPath)
            NSLog("Lexime: User history loaded from %@", historyPath)
            // Recovery report (§10): quarantine = user-visible data loss
            // (learning keeps running); everything else is log-only (§8 ※1:
            // tail repair is the expected power-loss residue, migration is
            // routine).
            let report = h.openReport()
            // Appended to every branch, not just the last one: both flags
            // leave checkpointState and walState at their healthy values, and
            // appendsFrozen can co-occur with a migration or a quarantine —
            // so a line built from the other fields alone reads exactly like
            // a clean startup. (migrationFailed cannot co-occur with
            // migratedFromV1, being defined as its negation; it is appended
            // uniformly rather than special-cased.)
            var degraded = ""
            if report.migrationFailed {
                degraded += " migration=failed(retrying via startup compaction)"
            }
            if report.appendsFrozen {
                degraded += " appends=frozen(memory-only until compaction)"
            }
            if report.dataLossSuspected {
                var detail =
                    "checkpoint: \(report.checkpointState), wal: \(report.walState)"
                if !report.quarantinedPaths.isEmpty {
                    detail += ", quarantined: \(report.quarantinedPaths.joined(separator: ", "))"
                }
                detail += degraded
                NSLog("Lexime: User history recovered with data loss (%@)", detail)
                failures.append(.historyDataLoss(detail: detail))
            } else if report.migratedFromV1 {
                NSLog(
                    "Lexime: User history migrated from v1 (\(report.framesReplayed) frames)\(degraded)"
                )
            } else if !report.clean {
                NSLog(
                    "Lexime: User history recovery events: checkpoint=\(report.checkpointState) wal=\(report.walState)\(degraded)"
                )
            }
            history = h
        } catch {
            NSLog("Lexime: Failed to open user history at %@: %@", historyPath, "\(error)")
            failures.append(.history(detail: "\(error)"))
            history = nil
        }

        let engine: LexEngine?
        if let dict {
            engine = LexEngine(dict: dict, conn: conn, history: history, userDict: userDict)
        } else {
            engine = nil
        }

        return EngineContainer(
            engine: engine, dictionary: dict, history: history, userDict: userDict,
            initFailures: failures)
    }
}
