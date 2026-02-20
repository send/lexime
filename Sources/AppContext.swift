import Foundation

class AppContext {
    static let shared = AppContext()

    let engine: LexEngine?
    let historyPath: String
    let userDictPath: String
    let supportDir: String
    let candidatePanel = CandidatePanel()

    private init() {
        guard let resourcePath = Bundle.main.resourcePath else {
            fatalError("Lexime: Bundle.main.resourcePath is nil")
        }

        // Load dictionary
        let dictPath = resourcePath + "/lexime.dict"
        var dict: LexDictionary?
        do {
            let d = try LexDictionary.open(path: dictPath)
            NSLog("Lexime: Dictionary loaded from %@", dictPath)
            let entries = d.lookup(reading: "かんじ")
            NSLog("Lexime: Sample lookup 'かんじ' → %d candidates", entries.count)
            dict = d
        } catch {
            NSLog("Lexime: Failed to load dictionary at %@: %@", dictPath, "\(error)")
            dict = nil
        }

        // Load connection matrix (optional — falls back to unigram if not found)
        let connPath = resourcePath + "/lexime.conn"
        let conn: LexConnection?
        do {
            let c = try LexConnection.open(path: connPath)
            NSLog("Lexime: Connection matrix loaded from %@", connPath)
            conn = c
        } catch {
            NSLog("Lexime: Connection matrix not found at %@ (using unigram fallback)", connPath)
            conn = nil
        }

        // Load user history (learning data)
        guard let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask).first else {
            fatalError("Lexime: Cannot find Application Support directory")
        }
        let leximeDir = appSupport.appendingPathComponent("Lexime").path
        self.supportDir = leximeDir
        self.historyPath = leximeDir + "/user_history.lxud"
        self.userDictPath = leximeDir + "/user_dict.lxuw"

        // Initialize tracing (no-op unless built with --features trace)
        let logDir = (NSSearchPathForDirectoriesInDomains(
            .libraryDirectory, .userDomainMask, true).first ?? "/tmp") + "/Logs/Lexime"
        try? FileManager.default.createDirectory(
            atPath: logDir, withIntermediateDirectories: true)
        traceInit(logDir: logDir)

        // Load custom settings if present
        let settingsPath = appSupport
            .appendingPathComponent("Lexime/settings.toml").path
        if FileManager.default.fileExists(atPath: settingsPath) {
            do {
                try settingsLoadConfig(path: settingsPath)
                NSLog("Lexime: Custom settings loaded from %@", settingsPath)
            } catch {
                NSLog("Lexime: settings config error at %@: %@",
                      settingsPath, "\(error)")
                // Embedded defaults will be used
            }
        }

        // Load custom romaji config if present
        let romajiPath = appSupport
            .appendingPathComponent("Lexime/romaji.toml").path
        if FileManager.default.fileExists(atPath: romajiPath) {
            do {
                try romajiLoadConfig(path: romajiPath)
                NSLog("Lexime: Custom romaji loaded from %@", romajiPath)
            } catch {
                NSLog("Lexime: romaji config error at %@: %@",
                      romajiPath, "\(error)")
                // Embedded default will be used
            }
        }

        // Load user dictionary (optional — for custom word registration)
        let userDict: LexUserDictionary?
        do {
            let ud = try LexUserDictionary.open(path: self.userDictPath)
            NSLog("Lexime: User dictionary loaded from %@", self.userDictPath)
            userDict = ud
        } catch {
            NSLog("Lexime: Failed to open user dictionary at %@: %@",
                  self.userDictPath, "\(error)")
            userDict = nil
        }

        // Reload dict with user dictionary layer if available
        if userDict != nil, dict != nil {
            do {
                let composite = try LexDictionary.openWithUserDict(
                    path: dictPath, userDict: userDict)
                NSLog("Lexime: Composite dictionary created (system + user)")
                dict = composite
            } catch {
                NSLog("Lexime: Failed to create composite dictionary: %@", "\(error)")
                // Fall back to system-only dict (already set)
            }
        }

        let history: LexUserHistory?
        do {
            let h = try LexUserHistory.open(path: self.historyPath)
            NSLog("Lexime: User history loaded from %@", self.historyPath)
            history = h
        } catch {
            NSLog("Lexime: Failed to open user history at %@: %@", self.historyPath, "\(error)")
            history = nil
        }

        // Load neural model (optional — GhostText mode requires this)
        let modelPath = resourcePath + "/zenz-v3.1-Q5_K_M.gguf"
        let neural: LexNeuralScorer?
        if FileManager.default.fileExists(atPath: modelPath) {
            do {
                let n = try LexNeuralScorer.open(modelPath: modelPath)
                NSLog("Lexime: Neural model loaded from %@", modelPath)
                neural = n
            } catch {
                NSLog("Lexime: Failed to load neural model at %@: %@", modelPath, "\(error)")
                neural = nil
            }
        } else {
            NSLog("Lexime: Neural model not found at %@ (GhostText mode unavailable)", modelPath)
            neural = nil
        }

        // Assemble engine (requires at least a dictionary)
        if let dict {
            self.engine = LexEngine(
                dict: dict, conn: conn, history: history, neural: neural,
                userDict: userDict)
        } else {
            self.engine = nil
        }
    }
}
