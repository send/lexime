import Foundation

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
            let store = try snippetsLoad(path: snippetPath)
            NSLog("Lexime: Snippets reloaded from %@", snippetPath)
            self.snippetStore = store
        } else {
            self.snippetStore = nil
        }
        NotificationCenter.default.post(name: .snippetsDidReload, object: nil)
    }
}
