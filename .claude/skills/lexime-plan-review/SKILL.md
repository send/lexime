---
name: lexime-plan-review
description: lexime pre-implementation plan-memo design review (5-agent parallel). Same 5 axes as /lexime-review applied to a plan BEFORE implementation starts. MANDATORY for work bundling ≥3 intersecting invariant axes or touching a subsystem with no canonical algorithm — catches architectural drift and review-tail explosions at the cheapest stage.
user-invocable: true
---

# lexime-plan-review — 実装前 plan review

`/lexime-review` の pre-impl 対。plan-memo 確定直後 / user approval 前に走らせ、実装後のレビューで発覚する設計問題と review tail をプラン段階で潰す。

- **Axis SSoT**: `../lexime-review/axes.md` (5 軸 + severity calibration を共有)
- 入力 = plan-memo のパス (または会話中のプラン本文)

## When to invoke

- **MANDATORY (CLAUDE.md §設計規律)**: 不変条件が交差する work — (a) **≥3 軸が交差する** (例: LXUD v2 = WAL × recovery × migration × fsync)、または (b) **正準アルゴリズムが無い subsystem** を触る場合。このとき単一 PR に束ねず **umbrella plan + per-PR plan に分割**し、各 per-PR plan に本 skill を通す。承認済み umbrella 配下で plan-review を通った narrowly-scoped per-PR slice は terminal 単位 (それ以上の再分割は要求しない)
- 推奨: 新機構の追加 / 既存機構の意味変更 / cross-crate 協調 / ユーザーデータ形式変更
- **判例: PR #281 (LXUD v2 PR1, 2026-07-02)** — 設計メモは max 品質だったが per-PR の edge-matrix を実装前に敷かず、実装 1 コミットに対しレビュー修正 37 コミットの発散ループを払った。この skill はその再発防止装置

## Skip OK

- plan-memo < 50 行 + 既存パターンの軽微 extension のみ
- trivial cleanup / rename で plan-memo 自体が不要なケース
- ⚠ MANDATORY trigger 該当時は skip 不可

## Workflow

1. plan-memo を読み、MANDATORY trigger 判定 (≥3 軸交差 / 正準アルゴリズム無し) を先に書面で行う。該当かつ分割されていなければ、**軸レビューの前に分割提案を出す**
2. Agent tool で 5 軸並列 (1 agent = 1 軸)。プロンプト template:

   > Read `<repo>/.claude/skills/lexime-review/axes.md` の Common + Axis N と `<repo>/CLAUDE.md`。次の plan-memo を Axis N の観点で精査せよ。実装 diff がまだ無いので「このプランを実装すると Axis N の Detect に該当する構造が生まれるか」「プランが該当ケースの扱いを未定義のまま残していないか (edge-matrix の穴)」を報告せよ。findings は `plan 該当箇所 / severity / 何が壊れる・何が未定義か一文 / 根拠`

3. 集約・severity 校正・disposition は `/lexime-review` Step 3 と同じ。プラン修正は実装より安い — real finding は plan に反映してから実装に進む
