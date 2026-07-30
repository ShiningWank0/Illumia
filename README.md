# Illumia

**Illumia** = **Illu**stration + **M**ed**ia**。
アニメ・2次元イラスト特化のセルフホスト画像閲覧アプリ。
Immich を基軸に、イラスト閲覧・漫画スタック・キャラクター認識
([anime_character_recognize](https://github.com/ShiningWank0/anime_character_recognize))
に特化する。

## 特徴 (設計済み・実装順に構築中)

- Google Photos / Immich ライクな justified タイルのタイムライン (日 / 月 / 年の 3 段ズーム)
- 漫画スタック: ページ順・話区切りを管理して作品として読める
- 重複アップロードの可視化と安全な自動整理・ゴミ箱 (復元可能)
- 非表示フォルダ (Vault): 暗号化 + パスワード / 生体認証。vault 内でも検索・ダウンロード可
- キャラクター認識・クラスタリング (オープンセット、ユーザー命名)
- 日本語検索 (ファイル名・キャラ名・スタック名。将来: OCR・スマートサーチ)
- 実行形態: Docker (NAS サーバー) / Web / Android APK / Windows・macOS
  (client-only 版と、サーバー内包で外部非公開の all-in-one 版)

## リポジトリ状態

現在は設計フェーズ完了・実装準備中。設計仕様は [docs/](docs/) が正。

| doc | 内容 |
|---|---|
| [01_architecture.md](docs/01_architecture.md) | 全体構成・クレート境界・実行形態 |
| [02_data_model.md](docs/02_data_model.md) | DB スキーマ・ライフサイクル |
| [03_api.md](docs/03_api.md) | REST/WS API 仕様 |
| [04_timeline_layout.md](docs/04_timeline_layout.md) | タイムライン・justified タイル |
| [05_manga_stack.md](docs/05_manga_stack.md) | 漫画スタック |
| [06_vault.md](docs/06_vault.md) | 非表示フォルダの暗号設計 |
| [07_ml_integration.md](docs/07_ml_integration.md) | ML サイドカー契約 |
| [08_clients.md](docs/08_clients.md) | クライアント仕様 |
| [09_dev_workflow.md](docs/09_dev_workflow.md) | 開発体制・CI ポリシー |
| [10_future_features.md](docs/10_future_features.md) | 将来的な機能追加プラン |
| [11_dedup_and_trash.md](docs/11_dedup_and_trash.md) | 重複・ゴミ箱・誤削除防止 |

## 開発

- テストビルドはローカル可、**本番ビルドは GitHub Actions のみ** (→ docs/09)
- Rust: rust-toolchain.toml で固定 / Python: **uv 必須** / Node: npm ci

## License

[LICENSE](LICENSE)
