# Illumia Android (Tauri 2)

`web/` の SvelteKit SPA (adapter-static) を WebView に包む Android クライアント。
アプリ ID: `com.shiningwank0.illumia`。詳細仕様は [docs/08_clients.md](../../docs/08_clients.md)。

## 構成

- `src-tauri/` … Tauri 2 の Rust シェル (独立 Cargo ワークスペース)。
  - `tauri.conf.json` … `frontendDist` は `../../../web/build` を指す。
  - `src/lib.rs` … `run()` (mobile entry)。http / dialog / fs / biometric プラグインを登録。
  - `capabilities/default.json` … プラグイン権限。
- `scripts/inject-signing.py` … CI が生成する gradle に release 署名を注入する。
- `package.json` … `@tauri-apps/cli` のみ (ビルド用)。

`src-tauri/gen/` (Android プロジェクト) と `src-tauri/target/` はコミットしない。
`gen/` は CI の `npm run tauri android init` で毎回生成する。

## ビルド (CI 前提)

APK の署名ビルドは GitHub Actions (`.github/workflows/release.yml` の `android` ジョブ)
でのみ行う。ローカルでは Android SDK/NDK が必要なため通常ビルドしない。

必要な Secrets:

- `ILLUMIA_ANDROID_KEYSTORE_B64` … keystore を base64 化したもの
- `ILLUMIA_ANDROID_KEYSTORE_PASSWORD`
- `ILLUMIA_ANDROID_KEY_ALIAS`

## アプリモード (web 側)

web の SPA は `window.__TAURI_INTERNALS__` で Tauri を検出し、アプリモードでのみ:

- サーバー接続設定 (external/local URL + 到達性プローブで自動選択)
- Bearer 認証 (device token は現状メモリ内保持 → Keystore 連携は将来)
- 生体認証による vault アンロック (フォールバックにパスワード入力を必ず残す)
- 自動アップロード (フォアグラウンド同期。バックグラウンド常駐は将来)

を有効化する。ブラウザ配信時は従来どおり同一オリジン Cookie 認証で動作する。
