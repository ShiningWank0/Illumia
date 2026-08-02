# Illumia Android (Tauri 2)

`web/` の SvelteKit SPA (adapter-static) を WebView に包む Android クライアント。
アプリ ID: `com.shiningwank0.illumia`。詳細仕様は [docs/08_clients.md](../../docs/08_clients.md)。

## 構成

- `src-tauri/` … Tauri 2 の Rust シェル (独立 Cargo ワークスペース)。
  - `tauri.conf.json` … `frontendDist` は `../../../web/build` を指す。
  - `src/lib.rs` … `run()` (mobile entry)。dialog / fs / biometric プラグインを登録。
  - `src/bridge.rs` … Illumia 専用 HTTP ブリッジ (`illumia_request` / `illumia_set_server`)。
  - `capabilities/default.json` … プラグイン権限。
  - `Cargo.lock` … コミット必須。release では `--locked` で解決を固定する (docs/12: SEC-007)。
- `scripts/inject-signing.py` … CI が生成する gradle に release 署名を注入する。
- `package.json` … `@tauri-apps/cli` のみ (ビルド用)。

`src-tauri/gen/` (Android プロジェクト) と `src-tauri/target/` はコミットしない。
`gen/` は CI の `npm run tauri android init` で毎回生成する。

## ビルド (CI 前提)

APK の署名ビルドは GitHub Actions (`.github/workflows/release.yml` の `android` ジョブ)
でのみ行う。ローカルでは Android SDK/NDK が必要なため通常ビルドしない。

必要な Secrets (作成手順は [SIGNING.md](SIGNING.md)):

- `ILLUMIA_ANDROID_KEYSTORE_B64` … keystore を base64 化したもの
- `ILLUMIA_ANDROID_KEYSTORE_PASSWORD`
- `ILLUMIA_ANDROID_KEY_ALIAS`

これらが未設定のままタグを push すると release の android ジョブが失敗する。
先に [SIGNING.md](SIGNING.md) の手順で登録すること。

## アプリモード (web 側)

web の SPA は `window.__TAURI_INTERNALS__` で Tauri を検出し、アプリモードでのみ:

- サーバー接続設定 (external/local URL + 到達性プローブで自動選択)
- Bearer 認証 (device token は現状メモリ内保持 → Keystore 連携は将来)
- 生体認証による vault アンロック (フォールバックにパスワード入力を必ず残す)
- 自動アップロード (フォアグラウンド同期。バックグラウンド常駐は将来)

を有効化する。ブラウザ配信時は従来どおり同一オリジン Cookie 認証で動作する。

## セキュリティ上の設計判断 (docs/12_security.md)

- **CSP を有効化** (SEC-003)。`tauri.conf.json` の `app.security.csp` に実効値を設定し、
  WebView 内で XSS が成立した場合の影響を抑える。ネットワークはブリッジ経由のみ、
  画像も blob 化して渡すため `connect-src` / `img-src` に外部 origin は不要。
- **汎用 HTTP プラグインを frontend へ公開しない** (SEC-004)。`plugin-http` を
  capability から外し、代わりに `src/bridge.rs` の専用コマンドだけを公開する。
  ブリッジは宛先を登録済み base URL と完全一致で検査し、path は `/api/` 配下のみ、
  method / header 名は allowlist、request / response body には上限を設ける。
  リダイレクト追従は無効 (別 host への誘導を塞ぐ)。
- **base URL は Rust 側でも検証する**。frontend の検証だけに依存しない。
  `https` のみ (平文 HTTP はプライベート宛先に限る)、credential 埋め込み・
  query・fragment・path・制御文字は拒否する。
- `fs:default` は現状アプリ固有ディレクトリ中心。将来 scope を追加する場合も
  選択済み media ディレクトリだけに限定すること。
