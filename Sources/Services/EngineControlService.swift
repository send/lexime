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
    /// that its row has been rendered. Called from the menu rather than from
    /// bootstrap: a launch that never shows a menu — an IMKit probe — must not
    /// consume a report on the user's behalf. Idempotent and non-blocking.
    func acknowledgeHistoryReport()
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
        try engine.clearHistory()
        // The engine unlinked the marker as part of the wipe; drop the row it
        // fed, or the menu keeps asking the user to re-delete an entry that no
        // longer exists.
        container.historyWasCleared()
    }

    func acknowledgeHistoryReport() {
        container.history?.ackOpenReport()
    }

    func historyDurabilityIssues() -> [LexHistoryDurabilityIssue] {
        // No history means learning never started — a startup failure the
        // container already latched as `.history`, not a durability problem.
        container.history?.durabilityIssues() ?? []
    }
}
