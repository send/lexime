import Foundation

/// The status rows the input-method menu shows above its normal items.
///
/// Two sources with different lifetimes feed it, and keeping them separate is
/// the point:
///
/// - `EngineInitFailure` latches. It records what went wrong while the engine
///   was starting, and nothing during the session retracts it.
/// - `LexHistoryDurabilityIssue` is polled and clearable. A frozen WAL thaws
///   when a compaction restores appendable form; an unpersisted deletion is
///   covered by the next durable checkpoint. Folding these into `initFailures`
///   would make a recovered disk keep warning forever.
///
/// The dividing line is retraction, not where the fact came from:
/// `.historyDeletionLost` is a durability failure too, but it is a *past* one,
/// so no amount of the disk recovering retracts it and it latches like the rest
/// of startup. It has exactly one retraction, and it is not the disk healing:
/// wiping the whole history makes the claim false rather than stale, so
/// `EngineContainer.historyWasCleared()` drops that row alone.
///
/// The other half of that separation is that a runtime issue must show even
/// when startup was clean — the main #295 scenario is a healthy launch
/// followed by a disk that fails hours later. So `menu()` gates on "are there
/// rows", never on whether the engine is degraded; gating on the latter would
/// display nothing in exactly the case this exists for.
enum DegradedStatus {

    /// One status row.
    struct Row: Equatable {
        let title: String
        /// Whether clicking the row acknowledges a durable record behind it.
        ///
        /// True for exactly one row. Every other row is derived — it either
        /// re-derives from live state on each menu open, or latches in memory
        /// and dies with the process — so there is nothing for a click to
        /// settle. The lost-deletion row is backed by a file that outlives the
        /// process, and only a person can say they have seen it: IMKit calls
        /// `menu()` on its own, without displaying anything, so "the menu was
        /// built" is not evidence of delivery. Verified on-device — the record
        /// was consumed four seconds after a relaunch nobody touched.
        let acknowledgeable: Bool
    }

    /// Status rows, init failures first. Empty when there is nothing to report.
    static func rows(
        initFailures: [EngineInitFailure],
        runtimeIssues: [LexHistoryDurabilityIssue]
    ) -> [Row] {
        initFailures.map { failure in
            Row(title: title(for: failure), acknowledgeable: isAcknowledgeable(failure))
        } + runtimeIssues.map { Row(title: title(for: $0), acknowledgeable: false) }
    }

    static func isAcknowledgeable(_ failure: EngineInitFailure) -> Bool {
        if case .historyDeletionLost = failure { return true }
        return false
    }

    static func title(for failure: EngineInitFailure) -> String {
        switch failure {
        case .dictionary:
            return NSLocalizedString(
                "⚠️ 辞書の読み込みに失敗（変換不可）",
                comment: "Degraded status: system dictionary failed")
        case .userDictionary:
            return NSLocalizedString(
                "⚠️ ユーザ辞書の読み込みに失敗",
                comment: "Degraded status: user dictionary failed")
        case .userDictionaryDataLoss:
            return NSLocalizedString(
                "⚠️ ユーザ辞書が破損していました（登録語を失いましたが登録は継続中）",
                comment: "Degraded status: user dictionary quarantined, words lost, registration continues")
        case .compositeDictionary:
            return NSLocalizedString(
                "⚠️ ユーザ辞書が変換に反映されていません",
                comment: "Degraded status: composite dictionary failed")
        case .history:
            return NSLocalizedString(
                "⚠️ 学習履歴の読み込みに失敗",
                comment: "Degraded status: user history failed")
        case .historyDataLoss:
            return NSLocalizedString(
                "⚠️ 学習履歴の一部を復旧できませんでした（学習は継続中）",
                comment: "Degraded status: user history partially lost, learning continues")
        case .historyDeletionLost:
            // Says 「前回」 and tells the user what to do, where the runtime row
            // below says only that a save is failing right now. Without that
            // split the two read as near-duplicates, and on a disk that is
            // still failing they appear together — the steady state, not a
            // corner. This is the one that has already happened: the entry is
            // back, and only the user can finish the job.
            return NSLocalizedString(
                "⚠️ 前回のセッションの削除が保存されていません（削除した内容が復元されている可能性があります。確認して再度削除してください）",
                comment: "Degraded status: a deletion from a previous session never reached disk")
        case .customSettings:
            return NSLocalizedString(
                "⚠️ 設定ファイルの読み込みに失敗（デフォルト設定で動作中）",
                comment: "Degraded status: custom settings failed")
        }
    }

    static func title(for issue: LexHistoryDurabilityIssue) -> String {
        switch issue {
        case .deletionNotPersisted:
            // Deliberately does not name restart as the risk. The issue covers
            // two halves: a tombstone whose frame never reached the WAL (where
            // restart does lose the deletion) and one whose frame is on disk
            // but whose flush failed (where restart is what *applies* it, via
            // replay). Naming restart would steer the second case exactly
            // wrong — toward avoiding the one action that helps.
            return NSLocalizedString(
                "⚠️ 削除した学習内容を保存できませんでした（削除が取り消される可能性があります）",
                comment: "Degraded status: a requested deletion may not have reached disk")
        case .learningMemoryOnly:
            // "新しい" is load-bearing: what is at risk is only what was
            // learned since the last checkpoint. A freeze inherited at open
            // can co-occur with `.historyDataLoss`'s 「学習は継続中」, and an
            // unqualified "learning is not being saved" next to that reads as
            // a contradiction — and overstates the loss besides.
            return NSLocalizedString(
                "⚠️ 新しい学習内容を保存できていません（再起動すると失われます）",
                comment: "Degraded status: learning since the last checkpoint is memory-only")
        }
    }
}
