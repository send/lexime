import Foundation

/// Engine-wide control operations exposed to the UI layer.
protocol EngineControlService {
    func clearHistory() throws
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
}
