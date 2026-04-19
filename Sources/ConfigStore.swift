import Foundation

struct SnippetLoadError: LocalizedError {
    let path: String
    let underlying: any Error
    var errorDescription: String? {
        "snippets at \(path): \(String(describing: underlying))"
    }
}

final class ConfigStore {
    let supportDir: String
    let userDictPath: String
    let snippetPath: String
    private(set) var snippetStore: LexSnippetStore?

    init(supportDir: String) {
        self.supportDir = supportDir
        self.userDictPath = (supportDir as NSString).appendingPathComponent("user_dict.lxuw")
        self.snippetPath = (supportDir as NSString).appendingPathComponent("snippets.toml")
        self.snippetStore = nil
    }

    /// Reload snippets from disk. Throws if the file exists but fails to load.
    /// On success or missing file, updates `snippetStore` and posts notification.
    func reloadSnippets() throws {
        if FileManager.default.fileExists(atPath: snippetPath) {
            do {
                let content = try String(contentsOfFile: snippetPath, encoding: .utf8)
                let entries = try SnippetTOML.parse(content)
                let store = try snippetsBuildStore(entries: entries)
                NSLog("Lexime: Snippets reloaded from %@", snippetPath)
                self.snippetStore = store
            } catch {
                throw SnippetLoadError(path: snippetPath, underlying: error)
            }
        } else {
            self.snippetStore = nil
        }
        NotificationCenter.default.post(name: .snippetsDidReload, object: nil)
    }
}
