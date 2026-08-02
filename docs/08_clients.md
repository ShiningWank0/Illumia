# 08. クライアント仕様 (Web / Android / デスクトップ)

## 共通

- UI 実体は `web/` の Svelte 5 SPA。Android (Tauri 2) は同じ SPA を包む。
  egui デスクトップのみ別実装 (サービス層 trait 経由 → docs/01)。
- 表示言語は日本語を第一とする。検索は日本語入力前提 (→ docs/03)。
- 対象: スマホ (縦)・iPad・PC の各画面幅にレスポンシブ対応。
  iPhone/iPad は Web (PWA 化は将来検討)。App Store / Play Store には公開しない。
- Web SPA は server と同一オリジンで配信する。認証は server が設定する `HttpOnly;
  SameSite=Strict; Path=/api` Cookie を使う。setup / login では
  `X-Illumia-Auth-Mode: cookie` を指定し、device token を response body として受け取らず、
  `localStorage` / `sessionStorage` / IndexedDB や URL へも保存しない。ログアウト時は
  server 側 token も失効する。
- ネイティブクライアントが受け取る device token は OS の secure storage
  (Android Keystore / macOS Keychain / Windows Credential Manager) にのみ保存する。
  平文設定ファイル・通常ログ・クラッシュレポートへ含めない。

## サーバー接続設定 (Web 以外のクライアント)

Immich モバイルアプリを参考にする。

- 初回起動でサーバー URL を入力 → `GET /api/server/info` で疎通確認 → ログイン。
- 初回セットアップでは、server が `setup_token_required=true` を返した場合だけ
  管理者が別経路で取得した setup token も入力する。setup token は保存しない。
- **ネットワーク別エンドポイント**: 1 つのサーバー登録に対して複数 URL を持てる。
  - `external`: 例 `https://illumia.example.com` (既定)。**`https` のみ**。平文 HTTP は
    設定ミスで credential を平文送信する経路になるため例外を設けない。
  - `local`: 例 `https://192.168.1.10:2283` (特定ネットワーク内でのみ有効)。
    平文 HTTP はプライベートアドレス宛のみ受理し、**自動選択しない**。
- 選択ロジック: 接続時に **external → local** の順で到達性プローブ (`/api/server/info`,
  timeout 2s) し、最初に検証を通った方を使う。セッション中に失敗したら再プローブ。
  - local を先に試すと、別ネットワーク上の攻撃者が同じ private IP で偽サーバーを
    立てるだけで採用され、共有パスワード・setup token・device token を奪える
    (→ docs/12_security.md)。このため **external を先に試す**。
  - local が平文 HTTP の場合は自動選択せず、接続のたびに利用者の明示確認を取る。
- **サーバー識別子の pin (TOFU)**: `/api/server/info` は `instance_id` (サーバー初回起動時に
  生成する乱数) を返す。クライアントは初回接続でこれを pin し、以後 pin と一致しない
  サーバーへは credential を一切送らない。到達性プローブは 2xx だけでは信用せず、
  response schema と `instance_id` の一致を確認する。
- Android では local URL に **Wi-Fi SSID を紐付け**るオプション: 指定 SSID に
  接続中のみ local を試す (位置情報権限が必要な旨を UI で説明)。
  - **M5 時点の縮退動作**: SSID を自動取得する Tauri プラグインが未整備のため、
    接続設定に SSID フィールド (手動メモ) は用意するが判定には使わず、上記の到達性
    プローブ (local→external, 各 timeout 2s) のみで自動選択する。SSID による分岐は
    プラグイン導入後に有効化する (→ docs/10)。

## 自動アップロード (Android / デスクトップ)

Immich のモバイルバックアップ相当。

- 対象フォルダ/アルバムを複数選択できる (Android: MediaStore のアルバム単位、
  デスクトップ: 任意フォルダ)。
- 検出方式:
  - Android: WorkManager の定期ジョブ + MediaStore 差分クエリ (Tauri プラグインとして実装)
  - デスクトップ (egui): notify クレートによるフォルダ監視 + 起動時フルスキャン
- 送信前に `POST /api/assets/exists` でハッシュ照合し、既存分はスキップ
  (重複としての記録が必要なケース = 手動アップロードとは区別し、自動アップロードでは
  純粋にスキップする)。
- クライアント側に送信済み台帳 (ローカル SQLite: path, hash, uploaded_at) を持ち、
  再起動・再インストール後も exists 照合で自己修復できること。
- 条件設定: Wi-Fi のみ / 充電中のみ (Android)。失敗は指数バックオフで再試行。
- Vault への自動アップロードは**対象外** (手動操作のみ)。
- **M5 時点の縮退動作**: v1 は「アプリ起動中のフォアグラウンド同期」に限定する
  (起動時 + 手動「今すぐ同期」)。WorkManager 定期ジョブ + MediaStore 差分クエリ・
  Wi-Fi/充電条件・指数バックオフ、および送信済み台帳のローカル SQLite 化は将来タスク
  (→ docs/10)。v1 は plugin-fs の readDir/readFile で扱えるフォルダのみ対象とし、
  台帳は localStorage (path→hash) で自己修復する。

## Android (Tauri 2)

- `apps/android/` の Tauri 2 プロジェクト。WebView に `web/` のビルドを同梱。
- APK は GitHub Actions で署名ビルド (→ .github/workflows/release.yml)。サイドロード配布。
- ネイティブ機能 (Tauri プラグイン):
  - 生体認証: tauri-plugin-biometric。**用途は vault アンロックの代替** (→ docs/06)。
    Android Keystore に「ラップ済み MK」を保存し、生体認証成功で取り出して unlock。
  - 自動アップロード用バックグラウンドワーカー / SSID 取得。
- 共有インテント (「Illumia へ送る」) は将来対応 (→ docs/10)。

## デスクトップ (egui, M6)

- 配布形態は 2 種 (同一アプリ名・起動時またはインストール時に選択):
  - **client-only**: リモートサーバーへ HTTP 接続。接続設定は上記共通仕様。
  - **all-in-one**: サーバー機能を同プロセスに内包。**TCP listener を持たず、
    アプリの外 (同一端末のブラウザ含む) からは一切アクセス不可** (→ docs/01)。
    データディレクトリはユーザー選択 (既定: OS 標準のアプリデータ位置)。
    ML サイドカーは PyInstaller 製バイナリを同梱し unix socket / named pipe で接続。
    ML を無効にして軽量運用も可 (`ml.enabled=false`)。
- 生体認証: macOS は Touch ID (LocalAuthentication + Keychain)。Windows Hello は将来検討。
- タイル UI は docs/04 と同仕様 (justified 実装を Rust に移植。同一のテストベクタを共有し
  web 版と結果一致を検証する)。

## ビューア共通仕様

- タイル → クリック/タップで全画面ビューア。左右スワイプ/キーで前後移動。
  ピンチズーム・ダブルタップズーム。
- 詳細パネル: メタデータ・所属スタック・show_in_timeline フラグ操作 (→ docs/05)・
  キャラクラスタ表示・vault へ移動・ダウンロード・削除 (→ docs/11)。
- 編集機能は提供しない (要件)。
