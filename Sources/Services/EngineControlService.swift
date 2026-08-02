import Foundation

/// Engine-wide control operations exposed to the UI layer.
protocol EngineControlService {
    func clearHistory() throws

    /// History durability problems that hold right now, most severe first.
    /// Polled rather than pushed: the engine has no seam to notify Swift from
    /// (the learning path runs on background threads with no session, and
    /// `LexSessionEvents` is the async candidate channel), and the sink is the
    /// status menu, which re-derives its rows on every open anyway.
    func historyDurabilityIssues() -> [LexHistoryDurabilityIssue]

    /// Retire the on-disk record behind a startup `deletionLost` report, now
    /// that its row has been shown to a person. Called from the menu row's
    /// action rather than from bootstrap or from building the menu: IMKit does
    /// both on its own, without displaying anything, so neither is evidence
    /// that the report reached anyone.
    ///
    /// Whether the row survives is `deletionReportOwed()`, not this call —
    /// see there. Idempotent, and never blocks on the engine's locks.
    func acknowledgeHistoryReport()

    /// Whether a lost-deletion report from a previous session is still owed.
    ///
    /// The single authority for whether the row belongs on screen. Both an
    /// acknowledgement the engine could not complete and a wipe that failed
    /// before its commit point leave it owed, so one question answers for both
    /// call sites.
    func deletionReportOwed() -> Bool
}

enum EngineControlServiceError: Error, LocalizedError {
    case engineUnavailable

    var errorDescription: String? {
        switch self {
        case .engineUnavailable:
            return "エンジンが利用できません"
        }
    }
}

final class DefaultEngineControlService: EngineControlService {
    private let container: EngineContainer

    init(container: EngineContainer) {
        self.container = container
    }

    func clearHistory() throws {
        guard let engine = container.engine else {
            throw EngineControlServiceError.engineUnavailable
        }
        // `defer`, because the wipe's commit point comes before the physical
        // steps whose failures it throws: a throw does not tell us whether the
        // report was retired. `retractRowIfSettled` asks the engine instead of
        // inferring it from control flow — the same rule the acknowledgement
        // uses, so the row has one authority rather than two.
        defer { retractRowIfSettled() }
        try engine.clearHistory()
    }

    func historyDurabilityIssues() -> [LexHistoryDurabilityIssue] {
        // No history means learning never started — a startup failure the
        // container already latched as `.history`, not a durability problem.
        container.history?.durabilityIssues() ?? []
    }

    func acknowledgeHistoryReport() {
        container.history?.ackOpenReport()
        retractRowIfSettled()
    }

    func deletionReportOwed() -> Bool {
        // No history means nothing was reported, so nothing is owed.
        container.history?.deletionReportOwed() ?? false
    }

    /// Drop the status row exactly when the engine no longer owes the report.
    private func retractRowIfSettled() {
        guard !deletionReportOwed() else { return }
        container.retractDeletionLostRow()
    }
}
