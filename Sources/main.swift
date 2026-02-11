import Cocoa
import InputMethodKit

let kConnectionName = "sh.send.inputmethod.Lexime_Connection"

guard let resourcePath = Bundle.main.resourcePath else {
    NSLog("Lexime: Bundle.main.resourcePath is nil")
    exit(1)
}

guard let bundleId = Bundle.main.bundleIdentifier else {
    NSLog("Lexime: Bundle.main.bundleIdentifier is nil")
    exit(1)
}

// Load dictionary once at startup
let sharedDict: OpaquePointer? = {
    let dictPath = resourcePath + "/lexime.dict"
    guard let dict = lex_dict_open(dictPath) else {
        NSLog("Lexime: Failed to load dictionary at %@", dictPath)
        return nil
    }
    NSLog("Lexime: Dictionary loaded from %@", dictPath)

    // Verify with a sample lookup
    let list = lex_dict_lookup(dict, "かんじ")
    NSLog("Lexime: Sample lookup 'かんじ' → %d candidates", list.len)
    lex_candidates_free(list)

    return dict
}()

// Load connection matrix (optional — falls back to unigram if not found)
let sharedConn: OpaquePointer? = {
    let connPath = resourcePath + "/lexime.conn"
    guard let conn = lex_conn_open(connPath) else {
        NSLog("Lexime: Connection matrix not found at %@ (using unigram fallback)", connPath)
        return nil
    }
    NSLog("Lexime: Connection matrix loaded from %@", connPath)
    return conn
}()

// Load user history (learning data)
let userHistoryPath: String = {
    let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    return appSupport.appendingPathComponent("Lexime/user_history.lxud").path
}()

let sharedHistory: OpaquePointer? = {
    guard let history = lex_history_open(userHistoryPath) else {
        NSLog("Lexime: Failed to open user history at %@", userHistoryPath)
        return nil
    }
    NSLog("Lexime: User history loaded from %@", userHistoryPath)
    return history
}()

guard let server = IMKServer(name: kConnectionName, bundleIdentifier: bundleId) else {
    NSLog("Lexime: Failed to create IMKServer")
    exit(1)
}

NSLog("Lexime: IMKServer started (connection: %@)", kConnectionName)

let sharedCandidatePanel = CandidatePanel()

_ = server  // keep the server alive

NSApplication.shared.run()
