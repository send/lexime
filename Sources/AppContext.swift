import Foundation

class AppContext {
    static let shared = AppContext()

    let dict: LexDictionary?
    let conn: LexConnection?
    let history: LexUserHistory?
    let neural: LexNeuralScorer?
    let historyPath: String
    let candidatePanel = CandidatePanel()

    private init() {
        guard let resourcePath = Bundle.main.resourcePath else {
            fatalError("Lexime: Bundle.main.resourcePath is nil")
        }

        // Load dictionary
        let dictPath = resourcePath + "/lexime.dict"
        do {
            let d = try LexDictionary.open(path: dictPath)
            NSLog("Lexime: Dictionary loaded from %@", dictPath)
            let entries = d.lookup(reading: "かんじ")
            NSLog("Lexime: Sample lookup 'かんじ' → %d candidates", entries.count)
            self.dict = d
        } catch {
            NSLog("Lexime: Failed to load dictionary at %@: %@", dictPath, "\(error)")
            self.dict = nil
        }

        // Load connection matrix (optional — falls back to unigram if not found)
        let connPath = resourcePath + "/lexime.conn"
        do {
            let c = try LexConnection.open(path: connPath)
            NSLog("Lexime: Connection matrix loaded from %@", connPath)
            self.conn = c
        } catch {
            NSLog("Lexime: Connection matrix not found at %@ (using unigram fallback)", connPath)
            self.conn = nil
        }

        // Load user history (learning data)
        guard let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask).first else {
            fatalError("Lexime: Cannot find Application Support directory")
        }
        self.historyPath = appSupport.appendingPathComponent("Lexime/user_history.lxud").path

        // Initialize tracing (no-op unless built with --features trace)
        let logDir = (NSSearchPathForDirectoriesInDomains(
            .libraryDirectory, .userDomainMask, true).first ?? "/tmp") + "/Logs/Lexime"
        try? FileManager.default.createDirectory(
            atPath: logDir, withIntermediateDirectories: true)
        traceInit(logDir: logDir)

        do {
            let h = try LexUserHistory.open(path: self.historyPath)
            NSLog("Lexime: User history loaded from %@", self.historyPath)
            self.history = h
        } catch {
            NSLog("Lexime: Failed to open user history at %@: %@", self.historyPath, "\(error)")
            self.history = nil
        }

        // Load neural model (optional — GhostText mode requires this)
        let modelPath = resourcePath + "/zenz-v3.1-Q5_K_M.gguf"
        if FileManager.default.fileExists(atPath: modelPath) {
            do {
                let n = try LexNeuralScorer.open(modelPath: modelPath)
                NSLog("Lexime: Neural model loaded from %@", modelPath)
                self.neural = n
            } catch {
                NSLog("Lexime: Failed to load neural model at %@: %@", modelPath, "\(error)")
                self.neural = nil
            }
        } else {
            NSLog("Lexime: Neural model not found at %@ (GhostText mode unavailable)", modelPath)
            self.neural = nil
        }
    }
}
