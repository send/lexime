import Foundation

/// Snippet file load/save operations exposed to the UI layer.
protocol SnippetService {
    func load() throws -> [LexSnippetEntry]
    func save(_ entries: [LexSnippetEntry]) throws
    func reload() throws
}

final class DefaultSnippetService: SnippetService {
    private let config: ConfigStore

    init(config: ConfigStore) {
        self.config = config
    }

    func load() throws -> [LexSnippetEntry] {
        let path = config.snippetPath
        guard let content = try? String(contentsOfFile: path, encoding: .utf8) else {
            return []
        }
        return try snippetsParse(content: content)
    }

    func save(_ entries: [LexSnippetEntry]) throws {
        let toml = snippetsSerialize(entries: entries)
        try FileManager.default.createDirectory(
            atPath: config.supportDir, withIntermediateDirectories: true)
        try toml.write(toFile: config.snippetPath, atomically: true, encoding: .utf8)
    }

    func reload() throws {
        try config.reloadSnippets()
    }
}
