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
/// followed by a disk that fails hours later. Nesting the runtime rows under
/// an `isDegraded` check would display nothing in exactly that case.
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
            return NSLocalizedString(
                "⚠️ 削除した学習内容を保存できませんでした（再起動で戻る可能性があります）",
                comment: "Degraded status: a requested deletion is not on disk and may resurrect")
        case .learningMemoryOnly:
            return NSLocalizedString(
                "⚠️ 学習内容を保存できていません（再起動すると失われます）",
                comment: "Degraded status: learning is memory-only until a compaction heals the WAL")
        }
    }
}
