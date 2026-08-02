import Foundation

final class EngineContainer {
    let engine: LexEngine?
    let dictionary: LexDictionary?
    let history: LexUserHistory?
    let userDict: LexUserDictionary?

    /// Initialization failures recorded for user visibility (menu / alert).
    private(set) var initFailures: [EngineInitFailure]

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

    /// The history failures a recovery report produces, as a pure function of
    /// the two independent facts it can carry.
    ///
    /// Extracted so the one property that matters here is testable: a lost
    /// deletion and a quarantine are independent facts about the same startup,
    /// so the lost-deletion case must not live inside the mutually exclusive
    /// branch chain that reports the quarantine — routing it through there
    /// would let `dataLossSuspected` mask it. Asserting that over
    /// `DegradedStatus.rows` cannot work: that function maps whatever list it
    /// is handed, so it would only be testing `Array.map`.
    static func historyFailures(
        deletionLost: Bool,
        dataLossSuspected: Bool,
        detail: String,
        deletionDetail: String
    ) -> [EngineInitFailure] {
        var failures: [EngineInitFailure] = []
        if deletionLost {
            failures.append(.historyDeletionLost(detail: deletionDetail))
        }
        if dataLossSuspected {
            failures.append(.historyDataLoss(detail: detail))
        }
        return failures
    }

    /// Drop the lost-deletion row, whether because a wipe made the claim false
    /// or because the user acknowledged it. The only latched row with a
    /// retraction, and both of its retractions are user actions.
    ///
    /// Both callers reach it through `EngineControlService.retractRowIfSettled`,
    /// which gates on the engine's own `deletionReportOwed()` rather than on
    /// control flow — so this stays a pure render-cache edit with no policy in
    /// it. (A `historyWasCleared()` wrapper used to sit in front of the wipe
    /// path; it never acquired a caller once the service took over, and the
    /// test that named it was pinning the wrapper instead of the gate.)
    func retractDeletionLostRow() {
        initFailures.removeAll {
            if case .historyDeletionLost = $0 { return true }
            return false
        }
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
            // Appended to every branch, not just the last one. migrationFailed
            // leaves checkpointState and walState at their healthy values, so
            // a line without it reads exactly like a clean startup.
            // appendsFrozen is the other way round — it usually accompanies
            // RepairFailed or Quarantined — but it also co-occurs with a
            // successful migration and with a quarantine, and those branches
            // return first. (migrationFailed cannot co-occur with
            // migratedFromV1, being its negation; it is appended uniformly
            // rather than special-cased.)
            // Built lazily: interpolating a UniFFI enum goes through Swift's
            // reflection runtime, and on a clean launch this would be the first
            // such call in the process — paying its one-time warmup in front of
            // the IME becoming responsive, for a string nothing then uses.
            func stateDetail() -> String {
                "checkpoint: \(report.checkpointState), wal: \(report.walState)"
            }
            var degraded = ""
            if report.migrationFailed {
                degraded += " migration=failed(v1 kept)"
            }
            if report.appendsFrozen {
                degraded += " appends=frozen(memory-only until compaction)"
            }
            // Appended outside the branch chain below, not inside it: the
            // chain is mutually exclusive, and a lost deletion co-occurs
            // freely with a quarantine (independent facts about the same
            // startup). Routing it through the chain would hide it behind
            // dataLossSuspected — which is also why it is a case of its own
            // and not folded into .historyDataLoss: that one means "past
            // learning was lost", this one means the opposite, data that
            // survived a deletion the user asked for (#312).
            var deletionDetail = ""
            if report.deletionLost {
                degraded += " deletion=lost(prior session, entry may be back)"
                deletionDetail = stateDetail() + degraded
                NSLog(
                    "Lexime: A deletion from a previous session was not persisted (%@)",
                    deletionDetail)
            }
            var quarantineDetail = ""
            if report.dataLossSuspected {
                quarantineDetail = stateDetail()
                if !report.quarantinedPaths.isEmpty {
                    quarantineDetail +=
                        ", quarantined: \(report.quarantinedPaths.joined(separator: ", "))"
                }
                quarantineDetail += degraded
                NSLog("Lexime: User history recovered with data loss (%@)", quarantineDetail)
            } else if report.migratedFromV1 {
                NSLog(
                    "Lexime: User history migrated from v1 (\(report.framesReplayed) frames)\(degraded)"
                )
            } else if !report.clean && !report.deletionLost {
                // deletionLost logged its own line above, carrying the same
                // `degraded` fragment — without this clause a lost deletion on
                // an otherwise healthy start says it twice.
                NSLog(
                    "Lexime: User history recovery events: checkpoint=\(report.checkpointState) wal=\(report.walState)\(degraded)"
                )
            }
            failures.append(
                contentsOf: EngineContainer.historyFailures(
                    deletionLost: report.deletionLost,
                    dataLossSuspected: report.dataLossSuspected,
                    detail: quarantineDetail,
                    deletionDetail: deletionDetail))
            // No ack here. `bootstrap()` runs on every process launch,
            // including the short-lived IMKit probe launches this controller
            // already designs around — none of which ever render a menu. Acking
            // at load would consume the report on the user's behalf and put
            // #295's gap back one layer up. `menu()` acks once the row is
            // actually on screen.
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
