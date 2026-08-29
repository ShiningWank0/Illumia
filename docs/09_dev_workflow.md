# 09. 開発体制・エージェント運用・CI ポリシー

本リポジトリは複数の AI コードエージェントで開発する。全エージェントは
着手前に docs/ の関連ドキュメントを読み、**仕様と実装が食い違う場合は docs を正**とする
(docs 側が古い場合は docs の更新を先に提案する)。

## 役割分担

| 役割 | 担当 |
|---|---|
| 設計・技術判断・レビュー・パフォーマンス最適化・全体監督 | Claude (Fable) — 監督セッション |
| コード実装の主力 | codex (sol high fast) / Claude Opus (high) |

サイクル: 監督が実装タスクを docs 参照付きで切り出す → 実装エージェントが書く →
監督がレビューし、問題箇所を修正 → コミット。

### モデル利用ポリシー
- Opus が 5h / 週次リミットに達しクレジット消費になる状況では、Opus の使用を止めて
  codex に切り替える。
- codex のトークンが切れたら**作業を中断してユーザーへ報告する** (勝手に別手段で続行しない)。

## コミット・ブランチ

- こまめに commit & push する (機能単位・レビュー通過単位)。
- M0 までは main 直コミット可。M1 以降の機能実装はフィーチャーブランチ + PR を基本とする。
- コミットメッセージ: `<type>: <要約>` (type: feat / fix / docs / refactor / test / ci / chore)。
  日本語可。

## ビルド・実行環境

- **テストビルドはローカル開発機で実施可** (`cargo test`, `cargo clippy`, `npm run build`,
  `uv run pytest` 等)。
- **本番ビルド (配布物: Docker イメージ・デスクトップバイナリ・APK) は GitHub Actions のみ**。
  ローカルでビルドした成果物を配布しないこと。
- Rust: `rust-toolchain.toml` でツールチェーン固定。依存の再現性は Cargo.lock (コミットする)。
- **Python は必ず uv 経由で実行する** (`uv sync`, `uv run ...`)。素の `pip` / `python` を
  直接使わない。ローカルのテスト実行も同様。
- Node: バージョンは `web/.nvmrc` と `package.json engines` で固定。`npm ci` を使う。

## コーディング規約

- Rust: `cargo fmt` 準拠、`cargo clippy -- -D warnings` を通すこと。`unsafe` は原則禁止
  (必要なら理由コメント + 監督レビュー必須)。
- TypeScript: `svelte-check` + eslint + prettier。`any` は原則禁止。
- Python (ml/): ruff (lint + format)、型ヒント必須 (mypy は将来導入)。
- パフォーマンス志向 (監督の担当領域): ホットパス (タイムライン API・サムネ生成・
  レイアウト計算) では割り当てとコピーを意識する。ただし早すぎる最適化より計測を優先し、
  ベンチ/計測結果をコミットメッセージか PR に残す。

## レビュー観点 (監督用チェックリスト)

1. docs との整合 (特に docs/11 の不変条件・docs/06 のログ抑制・docs/01 の all-in-one 非公開)
2. 削除系コードは必ず docs/11 の必須テストを伴うこと
3. vault 関連コードにファイル名・ID をログ出力していないこと
4. NAS 負荷: 新しい同期的重処理を API ハンドラに入れていないこと (ジョブキュー行き)
5. スキーマ変更はマイグレーション追加 + docs/02 更新をセットで

## テスト方針

- ユニット: 各クレート / web の vitest / ml の pytest。
- property test: justified レイアウト (→ docs/04)、削除・パージの不変条件 (→ docs/11)。
- 統合: サービス層に対する結合テスト (一時ディレクトリ + 実 SQLite)。
  ML はモックサイドカーで契約テスト (→ docs/07)。
- E2E (M1 以降): docker compose 起動 + 実ブラウザ (Playwright) で主要フロー。

## CI (→ .github/workflows/)

- `ci.yml`: push / PR で実行。paths-filter により変更のあったコンポーネントのみ
  lint + test を走らせる (docs のみの変更でも全体は green になる)。
  集約ジョブ `ci-ok` をブランチ保護の必須チェックにする。
- 全PR / mainのcheckout済みtreeをTrivy filesystem secret scannerで検査し、検出時は
  `ci-ok` を失敗させる。これはGitHub secret scanningの履歴検査を置き換えず、PRへ新しい
  credentialや秘密鍵を混入させないための追加gateとする。
- `codeql.yml`: main / PR / 週次で JavaScript/TypeScript、Python、Rust を CodeQL の
  `security-extended` query で解析する。結果の upload に必要な
  `security-events: write` 以外は read-only とし、Action は commit SHA へ固定する。
- `apps/android/**` は独立 Rust workspace / npm lockfile として専用 job で
  `cargo metadata --locked`、fmt、clippy、test、Android target に絞った `cargo audit`、
  `npm audit` を行う。Android-only 変更でもこの job と `ci-ok` を必ず通し、release の
  APK は同一 lockfile に対する full CI 成功後だけ build する。依存packageとGradle pluginを
  実行するbuild jobには署名鍵を渡さず、repositoryをcheckoutしない専用jobがunsigned artifact
  だけを `apksigner` で署名・検証する。
- Rust は `cargo audit`、Web は `npm audit`、Python は `uv export` + `pip-audit` を
  品質ゲートに含める。`cargo audit` の例外は `.cargo/audit.toml` に限定し、
  **修正版が存在せず、かつ脆弱性ではない勧告 (unmaintained 等) のみ**許可する。
  実際の脆弱性は ignore しない。追加時は理由と再評価条件をコメントで残す。Dockerに影響する変更は
  BuildKitで実imageをbuildし、TrivyでHIGH/CRITICALをscanする。
- `release.yml`: タグ `v*` / 手動 (workflow_dispatch) で本番ビルド。
  公開は `v*` tag の push event からだけ許可し、手動実行は選択refがtagでも常にdry-runとする。
  container は publish 時に1回だけbuildしてtag無しのcandidate digestとしてpushし、そのdigestを
  Trivyでscanする。全container candidateのscanと他の配布build/sign jobの成功後、単一jobが
  同じdigestへversion/`latest` tagを付ける。scan前に配布tagを付けたり、scan後に同じcontextを
  再buildした別imageを配布したり、matrix途中で一部imageだけを先行公開したりしない。
  コンポーネント未実装の間は preflight 判定で該当ジョブを自動スキップする。
  成果物: Docker イメージ (ghcr: server / ml)、egui バイナリ (macOS universal / Windows)、
  Android APK (署名は GitHub Secrets)。
- third-party Action はmutable tagではなくcommit SHAへ固定し、末尾commentに対応versionを
  残す。DependabotでCargo/npm/pip/Docker/GitHub Actionsを週次更新する。
- Dependabot の grouped update は minor / patch に限定する。major update は単独 PR とし、
  migration と互換性テストを別途レビューする。Node/Python/Rust の Docker toolchain は
  repository の固定versionと同時に更新し、Dockerfileだけを先行更新しない。
- release workflowのwrite権限はpackage push / release作成jobだけに付与する。
  containerにはSBOMとprovenanceを添付し、手動dev buildで `latest` を上書きしない。
- production Compose/TrueNAS 手順は release が記録した `image@sha256:<digest>` を必須とし、
  mutable tag へ暗黙 fallback させない。CI の Compose validation では test 用 digest を明示する。

## GitHub repository security settings

- `main` は ruleset / branch protection で PR 経由を必須にし、`ci-ok` と CodeQL の成功を
  required check にする。force-push と branch deletion は禁止する。
- vulnerability alerts / Dependabot alerts、secret scanning、private vulnerability reporting
  を有効化する。公開前に Security タブで未解決 alert が 0 件であることを確認する。
- Dependency Graphを有効化した後はPR差分のDependency Reviewもrequired checkへ追加する。
  有効化前もCargo/npm/Pythonの全lockfile監査を `ci-ok` 内で必須とする。
- 脆弱性報告は `SECURITY.md` に従い public Issue へ機密情報を書かない。
- `CODEOWNERS` は security-sensitive な workflow、Docker、認証、Vault、native bridge を
  repository owner のレビュー対象として明示する。
- Android署名secretは `release-signing` environmentにだけ登録し、公開releaseでは
  reviewer approvalを必須にする。通常のbuild jobへ署名secretを渡さない。
