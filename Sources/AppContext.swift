import Foundation

class AppContext {
    static let shared = AppContext()

    let dict: OpaquePointer?
    let conn: OpaquePointer?
    let history: OpaquePointer?
    let historyPath: String
    let candidatePanel = CandidatePanel()

    private init() {
        guard let resourcePath = Bundle.main.resourcePath else {
            fatalError("Lexime: Bundle.main.resourcePath is nil")
        }

        // Load dictionary
        let dictPath = resourcePath + "/lexime.dict"
        if let d = lex_dict_open(dictPath) {
            NSLog("Lexime: Dictionary loaded from %@", dictPath)
            let list = lex_dict_lookup(d, "かんじ")
            NSLog("Lexime: Sample lookup 'かんじ' → %d candidates", list.len)
            lex_candidates_free(list)
            self.dict = d
        } else {
            NSLog("Lexime: Failed to load dictionary at %@", dictPath)
            self.dict = nil
        }

        // Load connection matrix (optional — falls back to unigram if not found)
        let connPath = resourcePath + "/lexime.conn"
        if let c = lex_conn_open(connPath) {
            NSLog("Lexime: Connection matrix loaded from %@", connPath)
            self.conn = c
        } else {
            NSLog("Lexime: Connection matrix not found at %@ (using unigram fallback)", connPath)
            self.conn = nil
        }

        // Load user history (learning data)
        let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask).first!
        self.historyPath = appSupport.appendingPathComponent("Lexime/user_history.lxud").path

        if let h = lex_history_open(self.historyPath) {
            NSLog("Lexime: User history loaded from %@", self.historyPath)
            self.history = h
        } else {
            NSLog("Lexime: Failed to open user history at %@", self.historyPath)
            self.history = nil
        }
    }
}
