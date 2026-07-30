# エージェント向けガイド (codex / Claude 共通)

このリポジトリは複数の AI コードエージェントで開発される。以下を厳守すること。

1. **docs/ が正の仕様**。実装前に関連ドキュメントを読むこと。仕様と食い違う実装をしない。
   変更が必要なら docs の更新を先に提案する。ドキュメント索引は
   [docs/01_architecture.md](docs/01_architecture.md) の末尾にある。
2. **削除系コードは [docs/11_dedup_and_trash.md](docs/11_dedup_and_trash.md) の
   不変条件 I1〜I6 と必須テストに従う**。テストなしの削除コードは差し戻し対象。
3. **Vault 関連コードでファイル名・asset id をログに出さない**
   ([docs/06_vault.md](docs/06_vault.md))。
4. **all-in-one モードで TCP を listen しない** ([docs/01_architecture.md](docs/01_architecture.md))。
5. 重い処理を API ハンドラに書かない。ジョブキューへ
   ([docs/04_timeline_layout.md](docs/04_timeline_layout.md))。
6. Python は必ず uv 経由 (`uv sync` / `uv run`)。素の pip / python を使わない。
7. 本番ビルド成果物はローカルで作らない (GitHub Actions のみ)。
8. 品質ゲート: `cargo fmt` / `cargo clippy -- -D warnings` / `svelte-check` / `ruff` を通す。
9. コミットは機能単位でこまめに。規約は [docs/09_dev_workflow.md](docs/09_dev_workflow.md)。
