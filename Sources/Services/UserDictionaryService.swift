import Foundation

/// User-dictionary operations exposed to the UI layer.
protocol UserDictionaryService {
    func list() -> [LexUserWord]
    func register(reading: String, surface: String) throws
    func unregister(reading: String, surface: String) throws
    func save() throws
}

enum UserDictionaryServiceError: Error, LocalizedError {
    case engineUnavailable
    case duplicate
    case notFound

    var errorDescription: String? {
        switch self {
        case .engineUnavailable:
            return "エンジンが利用できません"
        case .duplicate:
            return "同じ読み・表層の単語が既に登録されています"
        case .notFound:
            return "指定された単語は登録されていません"
        }
    }
}

final class DefaultUserDictionaryService: UserDictionaryService {
    private let container: EngineContainer
    private let userDictPath: String

    init(container: EngineContainer, userDictPath: String) {
        self.container = container
        self.userDictPath = userDictPath
    }

    func list() -> [LexUserWord] {
        container.engine?.listUserWords() ?? []
    }

    func register(reading: String, surface: String) throws {
        guard let engine = container.engine else {
            throw UserDictionaryServiceError.engineUnavailable
        }
        let added = engine.registerWord(reading: reading, surface: surface)
        if !added {
            throw UserDictionaryServiceError.duplicate
        }
    }

    func unregister(reading: String, surface: String) throws {
        guard let engine = container.engine else {
            throw UserDictionaryServiceError.engineUnavailable
        }
        let removed = engine.unregisterWord(reading: reading, surface: surface)
        if !removed {
            throw UserDictionaryServiceError.notFound
        }
    }

    func save() throws {
        guard let engine = container.engine else {
            throw UserDictionaryServiceError.engineUnavailable
        }
        try engine.saveUserDict(path: userDictPath)
    }
}
