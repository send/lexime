import Foundation

final class EngineContainer {
    let engine: LexEngine?
    let dictionary: LexDictionary?
    let history: LexUserHistory?
    let userDict: LexUserDictionary?

    init(
        engine: LexEngine?,
        dictionary: LexDictionary?,
        history: LexUserHistory?,
        userDict: LexUserDictionary?
    ) {
        self.engine = engine
        self.dictionary = dictionary
        self.history = history
        self.userDict = userDict
    }

    static func load(resourcePath: String, userDictPath: String, historyPath: String) -> EngineContainer {
        let dictPath = (resourcePath as NSString).appendingPathComponent("lexime.dict")
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

        let connPath = (resourcePath as NSString).appendingPathComponent("lexime.conn")
        let conn: LexConnection?
        do {
            let c = try LexConnection.open(path: connPath)
            NSLog("Lexime: Connection matrix loaded from %@", connPath)
            conn = c
        } catch {
            NSLog("Lexime: Connection matrix not found at %@ (using unigram fallback)", connPath)
            conn = nil
        }

        let userDict: LexUserDictionary?
        do {
            let ud = try LexUserDictionary.open(path: userDictPath)
            NSLog("Lexime: User dictionary loaded from %@", userDictPath)
            userDict = ud
        } catch {
            NSLog("Lexime: Failed to open user dictionary at %@: %@", userDictPath, "\(error)")
            userDict = nil
        }

        if userDict != nil, dict != nil {
            do {
                let composite = try LexDictionary.openWithUserDict(
                    path: dictPath, userDict: userDict)
                NSLog("Lexime: Composite dictionary created (system + user)")
                dict = composite
            } catch {
                NSLog("Lexime: Failed to create composite dictionary: %@", "\(error)")
            }
        }

        let history: LexUserHistory?
        do {
            let h = try LexUserHistory.open(path: historyPath)
            NSLog("Lexime: User history loaded from %@", historyPath)
            history = h
        } catch {
            NSLog("Lexime: Failed to open user history at %@: %@", historyPath, "\(error)")
            history = nil
        }

        let engine: LexEngine?
        if let dict {
            engine = LexEngine(dict: dict, conn: conn, history: history, userDict: userDict)
        } else {
            engine = nil
        }

        return EngineContainer(
            engine: engine, dictionary: dict, history: history, userDict: userDict)
    }
}
