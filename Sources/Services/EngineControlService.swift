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
    }

    func historyDurabilityIssues() -> [LexHistoryDurabilityIssue] {
        // No history means learning never started — a startup failure the
        // container already latched as `.history`, not a durability problem.
        container.history?.durabilityIssues() ?? []
    }
}
