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
/// The other half of that separation is that a runtime issue must show even
/// when startup was clean — the main #295 scenario is a healthy launch
/// followed by a disk that fails hours later. So `menu()` gates on "are there
/// rows", never on whether the engine is degraded; gating on the latter would
/// display nothing in exactly the case this exists for.
enum DegradedStatus {

    /// Titles for the disabled status rows, init failures first.
    /// Empty when there is nothing to report.
    static func rows(
        initFailures: [EngineInitFailure],
        runtimeIssues: [LexHistoryDurabilityIssue]
    ) -> [String] {
        initFailures.map(title(for:)) + runtimeIssues.map(title(for:))
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
