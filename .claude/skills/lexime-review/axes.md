# lexime-review axes — 検査軸 SSoT

`/lexime-review` (diff 対象) と `/lexime-plan-review` (plan-memo 対象) が共有する 5 軸。導出元は CLAUDE.md §設計哲学 / §設計規律。各軸の判例 (PR 番号つき逆引き) は memory `project_skill_buildout_plan.md` を参照。

Detect リストは **fail-safe な非網羅** — リストにない形でも軸の趣旨に反していれば flag する。迷ったら flag (over-run は安全、wrongly-skip は危険)。

## Common: severity calibration

- **CRITICAL**: 変換品質・ユーザーデータ・入力体験を実際に壊す。不変条件違反
- **IMPORTANT**: 「何が具体的に壊れるか」を一文で言える設計・正しさの問題
- **MINOR**: 上記一文テストを通らない polish。doc の不正確さは誤誘導する場合のみ IMPORTANT
- 補助判定: 「これはユーザーの思考を止めるか?」(Speed of Thought)
- dev tool (`dictool` 等) の性能指摘は原則 flag しない (CLAUDE.md: hot path のみが性能対象)

## Axis 1 — 辞書ソースオブトゥルース

言語知識がコードに漏れていないか。

### Detect

- 特定の surface / reading リテラルへの分岐・特例をコードに追加している
- 変換ミスをコードで直している (辞書エントリ / コスト / corpus ケースで直せる形なのに)
- 分節・Viterbi の構造問題をコスト調整で偽装している
- POS で駆動できる分類をリテラル列挙で実装している
- 辞書拡充が curated 契約を迂回している (bulk 投入、out-of-build 候補プールの自動昇格)

## Axis 2 — 測定・コーパス規律

品質変更が測定で正当化されているか。

### Detect

- コスト・重み・reranker 変更に accuracy 両コーパスの before/after が無い
- 「直る側」の flip のみ提示し「壊れる側」の反実仮想を実測していない
- 変換精度バグの修正に regression corpus ケースが無い
- history コーパス新規ケースに `baseline` が無い / corpus `skip` に issue リンクが無い
- hot path に影響し得る変更で速度への言及が無い (speed baseline 比較)

## Axis 3 — 構造的正しさ

正しさが規約でなく構造で守られているか。

### Detect

- lock guard を callback / FFI 呼び出し越しに保持している
- 世代整合を epoch / watermark を通さずに素通ししている (stale 候補の混入経路)
- ユーザーデータ書き込みに耐久性契約が無い (fsync / 原子 rename / tombstone / migration)
- 複数ファイル・複数状態の更新がトランザクショナルでない (途中クラッシュで不整合)
- 失敗を握りつぶしている (ユーザーデータ・エンジン初期化系の catch + log のみ)
- 「〜しない慣習」だけで守られる新しい正しさを導入している (構造化の検討痕跡なし)

## Axis 4 — UniFFI 境界

Rust ↔ Swift 境界の規律。

### Detect

- panic し得るパスが FFI 境界に露出している (`unwrap` / `expect` / 添字 in `engine/src/`)
- IMKit main thread をブロックし得る同期呼び出しの追加
- poll / timer ベースのイベント取得の再導入 (push/callback が正準)
- Swift 側で protocol シームを迂回して engine 型を View 層に露出
- Rust と Swift に同じ真実の二重実装 (Rust 単一真実源の violation)

## Axis 5 — プロジェクト文脈整合

過去の判断・進行中の作業との整合。

### Detect

- 単一正準形の violation: 既存機構と並行する新実装 (既知の未収束: コスト契約 /
  resegment・tune 並行実装 / settings 3 重定義 — 触ったなら収束方向か)
- 撤退済みパターンの再導入 (bulk merge / poll / committed_context 型の読者なし蓄積)
- 不変条件が交差する work の単一 PR 束ね (multi-PR + plan review 規律の迂回)
- 挙動変更が SPEC.md と矛盾したまま (仕様書 drift)
- 見送り済み大型 refactor への無断踏み込み (発動条件は記録に従う)
