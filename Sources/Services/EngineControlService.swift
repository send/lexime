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
    /// consume a report on the user's behalf.
    ///
    /// Returns whether the record is now retired. `false` means keep the row:
    /// the acknowledgement did not take, and the row is the only way to retry.
    ///
    /// Idempotent. Never blocks on the engine's locks (a contended call is
    /// skipped and retried on the next menu open); it does perform one unlink.
    @discardableResult
    func acknowledgeHistoryReport() -> Bool
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
        // `defer`, not a statement after the call. The engine unlinks the
        // marker at the wipe's commit point and *then* runs physical steps
        // whose failures it surfaces as a throw — so the throwing path is
        // exactly the one where the marker is already gone and the session
        // continues (the reset flow only restarts the process when nothing
        // failed). Retracting only on success would fire where it is
        // redundant and skip where it is needed.
        //
        // The residue: a clear that fails *before* its commit point retracts
        // the row a session early. The marker survives that path, so the next
        // launch reports again — chosen over leaving a standing instruction to
        // delete an entry from a history that was just wiped.
        defer { container.historyWasCleared() }
        try engine.clearHistory()
    }

    @discardableResult
    func acknowledgeHistoryReport() -> Bool {
        // No history means nothing was reported and there is no row to keep.
        container.history?.ackOpenReport() ?? true
    }

    func historyDurabilityIssues() -> [LexHistoryDurabilityIssue] {
        // No history means learning never started — a startup failure the
        // container already latched as `.history`, not a durability problem.
        container.history?.durabilityIssues() ?? []
    }
}
