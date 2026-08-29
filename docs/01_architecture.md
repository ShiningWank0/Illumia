# 01. 全体アーキテクチャ

Illumia は Immich を参考にした、アニメ・2次元イラスト特化のセルフホスト画像閲覧アプリ。
本ドキュメント群は実装エージェント(codex / Claude)が参照する正の設計仕様である。
仕様変更は必ずこの docs/ を先に更新してから実装する。

## 構成要素

```
┌─ クライアント ────────────────────────────────┐
│ Web (ブラウザ: PC/iPad/スマホ) ─ Svelte 5 SPA    │
│ Android APK ─ Tauri 2 (同じ SPA + 生体認証等)    │
│ Win/mac ─ egui ネイティブ                        │
│   ├ client-only 版                              │
│   └ all-in-one 版 (server crate を同プロセス埋込) │
└──────────────┬─────────────────────────────┘
               │ REST + WebSocket(ジョブ進捗)      → docs/03_api.md
┌─ サーバー ────▼─────────────────────────────┐
│ illumia-server (Rust / axum)                    │
│  ├ 静的配信 (Web SPA)                           │
│  ├ 取り込み・サムネイル生成・タイムライン API      │
│  ├ ジョブキュー (SQLite 実装, 並列度制限)         │
│  ├ 認証・Vault(非表示フォルダ)暗号化              │
│  └ ML オーケストレーション                        │
│        │ HTTP over unix socket / localhost       │
│ illumia-ml (Python サイドカー, ステートレス)       │
│  └ anime_character_recognize を依存として利用     │
└────────────────────────────────────────────┘
```

- **サーバー実行環境の想定**: NAS (TrueNAS, i3-14100 = 4C8T, GPU なし)。
  重い処理(サムネ生成・ML)は必ずジョブキュー経由のバックグラウンド実行とし、
  API 応答と UI 描画を阻害しない。レイアウト計算等はクライアント側に寄せる
  (→ docs/04_timeline_layout.md)。
- **ML 結果を含む全ての永続状態は Rust 側 DB が正**。Python サイドカーは
  ステートレス (→ docs/07_ml_integration.md)。

## リポジトリ構成 (モノレポ)

```
Illumia/
  Cargo.toml            # cargo workspace
  rust-toolchain.toml   # ツールチェーン固定 (再現性は cargo + Cargo.lock が担う)
  crates/
    illumia-core/       # ドメインロジック・DB スキーマ/マイグレーション・暗号・ジョブ
    illumia-server/     # axum バイナリ + lib (all-in-one 埋込用)
    illumia-desktop/    # egui アプリ (M6)
  web/                  # SvelteKit SPA (adapter-static, SPA モード)
  apps/android/         # Tauri 2 プロジェクト (web/ のビルド成果物を取り込む)
  ml/                   # Python サイドカー (uv 管理)
  docker/               # Dockerfile.*, production compose + digest wrapper, dev build overlay
  docs/                 # 本設計ドキュメント群
  .github/workflows/    # CI / 本番ビルド
```

## クレート境界

- **illumia-core**: フレームワーク非依存のドメイン層。
  - DB アクセス (rusqlite + マイグレーション)、エンティティ、ライフサイクル遷移
    (→ docs/02, docs/11)、Vault 暗号 (→ docs/06)、ジョブキュー、ML クライアント。
  - **サービス層 trait 群** (`AssetService`, `TimelineService`, `StackService`,
    `VaultService`, `SearchService`, ...) をここで定義する。
- **illumia-server**: core のサービス層を axum の HTTP ハンドラに束ねるだけの薄い層 + SPA 静的配信。
  `lib` としてもビルドでき、バイナリ `illumia-server` は lib の起動関数を呼ぶだけにする。
- **illumia-desktop**: egui クライアント。
  - client-only モード: HTTP で リモート server に接続。
  - all-in-one モード: illumia-core のサービス層を **in-process で直接呼び出す**。

### all-in-one 版は完全クローズド (必須要件)

- all-in-one モードでは **TCP ポートを一切 bind しない**。HTTP listener は存在しない。
  同一端末のブラウザからも他端末からもアクセスできないことを構造的に保証する
  (「localhost で listen して認証で弾く」方式は禁止)。
- これを可能にするため、egui クライアントは HTTP クライアントではなく
  サービス層 trait (`Arc<dyn AssetService>` 等) に依存する。実装が
  「リモート HTTP 実装」か「ローカル direct 実装」かで client-only / all-in-one を切り替える。
- ML サイドカーとの通信も unix domain socket (Windows は named pipe) を用い、TCP を開かない。
- CI の統合テストで「all-in-one 起動中に listen 中の TCP ソケットが存在しないこと」を検証する。

## データディレクトリ (サーバー側)

```
<data_root>/
  illumia.db            # メイン DB (SQLite, WAL)
  vault/                # 非表示フォルダ (→ docs/06)
    vault.db            # SQLCipher
    vault.keyfile       # KDF salt + ラップ済みマスターキー
    blobs/              # 暗号化済み画像・サムネイル
  library/<yyyy>/<mm>/<asset_id>.<ext>   # オリジナル画像 (asset 毎に 1 ファイル所有)
  thumbs/<asset_id>_t.webp               # 240px サムネイル
  thumbs/<asset_id>_p.webp               # 1440px プレビュー
```

ML モデルは application data と同じ tree に置かない。Docker では独立した
`illumia_models` volume を ML container の `/models` へ read-only mount し、desktop でも
`<data_root>` 外の明示した model root を使う。ML sidecar に `<data_root>` を mount しては
ならない。

- ファイルは **asset 行が 1 対 1 で所有**する。重複アップロードでも物理ファイルを共有しない
  (refcount 事故で本体を失うリスクを排除。ディスク増は重複保持期間で自然回収。→ docs/11)。

## 技術スタック確定事項

| 領域 | 採用 | 備考 |
|---|---|---|
| サーバー | Rust stable / axum / tokio | rust-toolchain.toml で固定 |
| DB | rusqlite (bundled) + SQLCipher (vault) | ORM 不使用。マイグレーションは自前の versioned SQL |
| 画像処理 | image + zune-jpeg + fast_image_resize + webp | サムネは WebP 出力 |
| ハッシュ | BLAKE3 (32B) | 重複判定・整合性検証 |
| Web | Svelte 5 / SvelteKit (adapter-static) + TypeScript | Immich (SvelteKit) を参考実装にできる |
| Android | Tauri 2 | APK 配布。App Store 非公開 |
| デスクトップ | egui / eframe | gpui は Windows 対応安定後に再評価 (→ docs/10) |
| ML | Python 3.12 / uv / FastAPI / onnxruntime | v1 のみ。将来 Rust (ort) 移行 (→ docs/10) |
| 検索 | SQLite FTS5 (trigram tokenizer) | 日本語部分一致必須 (→ docs/03) |

## 実行形態マトリクス

| 形態 | サーバー | クライアント | 備考 |
|---|---|---|---|
| Docker (TrueNAS 等) | ○ | × | サーバー専用。compose で server + ml |
| Web ブラウザ | × | ○ | server が SPA を配信 |
| Android APK | × | ○ | Tauri 2 |
| Win/mac client-only | × | ○ | egui |
| Win/mac all-in-one | ○(内包・非公開) | ○ | listener なし。外部アクセス不可 |

## ドキュメント索引

| doc | 内容 |
|---|---|
| 02_data_model.md | DB スキーマ・ライフサイクル・不変条件 |
| 03_api.md | REST/WS API 仕様 |
| 04_timeline_layout.md | タイムライン・justified タイル・仮想スクロール |
| 05_manga_stack.md | 漫画スタック仕様 |
| 06_vault.md | 非表示フォルダの暗号設計 |
| 07_ml_integration.md | ML サイドカー RPC 契約 |
| 08_clients.md | クライアント仕様 (接続設定・自動アップロード・生体認証) |
| 09_dev_workflow.md | 開発体制・エージェント運用・CI ポリシー |
| 10_future_features.md | 将来的な機能追加プラン |
| 11_dedup_and_trash.md | 重複管理・ゴミ箱・誤削除防止の不変条件 |
| 12_security.md | 脅威モデル・認証境界・インターネット公開時の必須要件 |
| 13_model_requirements.md | ML モデルバンドルの要件・配置・較正手順 (ACR 契約) |
| 14_install.md | プラットフォーム別のインストール・使い方 (Docker / Web / Android APK / デスクトップ) |
| 15_release_evidence.md | v1公開前の実環境検証記録とrelease承認手順 |
