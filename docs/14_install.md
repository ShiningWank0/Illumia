# 14. インストールと使い方 (プラットフォーム別)

Illumia は「サーバー 1 台 + 各端末のクライアント」という構成で使う。
まずサーバーを立て、そのあとで各端末から接続する。

| 形態 | 役割 | 配布物 |
|---|---|---|
| Docker (TrueNAS 等) | **サーバー** | GHCR のdigest固定イメージ |
| Web ブラウザ | クライアント | サーバーが配信 (追加インストール不要) |
| Android | クライアント | GitHub Release の APK (サイドロード) |
| macOS / Windows | クライアント / all-in-one | 現状は自前ビルド (下記参照) |

> リポジトリとGitHub Releaseはpublic。GHCR packageの公開状態はrelease notesを正とし、
> pullが401になる環境でだけ `read:packages` 権限のPersonal Access Token (PAT)を使う。

---

## 1. サーバー (Docker)

TrueNAS 等の常時稼働機に立てる。詳細な運用手順は
[docker/README.md](../docker/README.md)、セキュリティ要件は
[docs/12_security.md](12_security.md) を参照。

### 1-1. GHCR の認証を確認する

public packageはログイン不要でpullできる。401が返る場合はGitHubの
Settings → Developer settings → Personal access tokensで
`read:packages` 権限のPATを作り、次のようにログインする。

```bash
echo "<あなたのPAT>" | docker login ghcr.io -u <GitHubユーザー名> --password-stdin
```

### 1-2. compose ファイルと .env を用意する

```bash
git clone https://github.com/ShiningWank0/Illumia.git
cd Illumia
cp docker/.env.example docker/.env
```

`docker/.env` を編集し、`ILLUMIA_SETUP_TOKEN` に 32 文字以上のランダム値を設定する
(初回セットアップの横取りを防ぐため → docs/12)。
さらに Release notes に記載された server/ML の immutable digest を
`ILLUMIA_SERVER_DIGEST` / `ILLUMIA_ML_DIGEST` へ Release notes の64桁 digest 本体を設定する。
repository と `@sha256:` は production Compose 内で固定し、環境変数から変更できない。
`.env.example` の zero digest は安全な placeholder であり、そのままでは pull に失敗する。

```bash
# 十分にランダムな setup token を生成する例
openssl rand -hex 32
```

**権限を必ず絞る** (docs/12 が要求。世界読み取り可のままにしない):

```bash
chmod 600 docker/.env
```

### 1-3. 起動する

```bash
./docker/compose-prod.sh --profile ml up -d
```

ML (キャラクター認識) を使わない場合は `--profile ml` を外す。

- ホストポートは既定で `127.0.0.1` のみに bind する。
  インターネット公開は必ず Pangolin/Newt 等のリバースプロキシ経由にすること。
- 初回セットアップはリバースプロキシで公開する**前**に済ませることを推奨。

### 1-4. 初期セットアップ

ブラウザで `http://127.0.0.1:2283` を開き、パスワードを設定する。
`ILLUMIA_SETUP_TOKEN` を設定した場合は、その値も入力する。

セットアップ完了後は `docker/.env` から setup token を削除するか rotate し、
コンテナを再作成する。

### 1-5. 更新する

```bash
./docker/compose-prod.sh --profile ml pull
./docker/compose-prod.sh --profile ml up -d
```

更新時は Release notes の server/ML digest を確認し、`docker/.env` の両値を明示的に更新してから
pull する。mutable tag や `latest` は production 手順では使用しない。
wrapper は digest 形式を検証し、`--no-build` で production Compose を起動する。

---

## 2. Web ブラウザ (PC / iPhone / iPad / Android)

**インストール不要。** サーバーの URL をブラウザで開くだけで使える。
サーバーが SPA を同一オリジンで配信し、認証は HttpOnly Cookie で行う。

- 対応: スマホ (縦)・iPad・PC の各画面幅
- iPhone / iPad はこの Web 版を使う (App Store には公開しない)
- 外部公開している場合は `https://<あなたのドメイン>` を開く

---

## 3. Android (APK のサイドロード)

Google Play には公開しないため、APK を直接インストールする。

### 3-1. APK をダウンロードする

publicなGitHub Releaseからブラウザで取得する。

1. <https://github.com/ShiningWank0/Illumia/releases/latest> を開く。
2. `Assets` を展開し、`app-universal-release.apk` をタップする。
3. 「この種類のファイルは端末に損害を与える可能性があります」と出るが、
   自分でビルドした APK なので `OK` / `ダウンロード` を選ぶ。

ダウンロード後は必ず3-2のハッシュと署名fingerprintを照合すること。

### 3-2. 配布物が本物か確認する (推奨)

Release notes に APK の SHA-256 と署名証明書の fingerprint を載せている。
ダウンロードしたファイルと突き合わせる。

```bash
shasum -a 256 app-universal-release.apk
```

Android SDK があれば署名も確認できる:

```bash
apksigner verify --print-certs app-universal-release.apk
```

表示された証明書の SHA-256 digest が Release notes の値と一致すれば、
あなたの署名鍵で作られた APK である。

### 3-3. インストールする

Android は既定で「提供元不明のアプリ」を拒否する。

1. ダウンロードした APK をタップする。
2. 「不明なアプリのインストール」の許可を求められたら、
   **設定 → このアプリ (Chrome 等) に許可** を有効にする。
3. 戻ってインストールを実行する。

`adb` が使える場合はこちらでもよい:

```bash
adb install -r app-universal-release.apk
```

### 3-4. 初回起動 — サーバーへの接続

1. アプリを開くと接続設定画面が出る。
2. **外部 URL (external)**: `https://illumia.example.com` のように入力する。
   **`https` のみ**受け付ける (平文 HTTP は認証情報が漏れるため不可)。
3. **ローカル URL (local, 任意)**: 自宅 LAN 用。可能なら `https` にする。
   平文 HTTP のローカル URL は**自動では使われず**、接続のたびに確認を求める。
4. 「接続」を押すとサーバーへ疎通確認し、初回はサーバー識別子を記憶する
   (以後、別のサーバーが応答してもパスワードを送らない → docs/12 SEC-002)。
5. パスワードでログインする。

> **注意**: 更新時は同じ署名鍵の APK でしか上書きインストールできない。
> 別の鍵で署名し直した場合は、一度アンインストールが必要になる。

### 3-5. できること

- タイムライン閲覧・ビューア・漫画スタック・検索・人物クラスタ・Vault
- Vault のパスワードアンロック (生体認証は専用 Keystore 実装が完成するまで無効)
- フォアグラウンドでの自動アップロード (アプリ起動中 + 手動「今すぐ同期」)

バックグラウンド常駐での自動アップロードは未対応 (→ docs/10)。

---

## 4. macOS / Windows デスクトップ (egui)

### 現状: 配布物はありません

**署名・公証が未導入のため、GitHub Release にバイナリを添付していません**
(docs/12 SEC-007)。未署名バイナリを配ると、受け取った側が改ざんを検出できず、
macOS では Gatekeeper に、Windows では SmartScreen にブロックされます。

使うにはソースからビルドしてください。

### 4-1. ビルドする

Rust ツールチェーンが必要 (`rust-toolchain.toml` で固定)。

```bash
git clone https://github.com/ShiningWank0/Illumia.git
cd Illumia
cargo build --release -p illumia-desktop
```

Linux では追加で開発ヘッダが要ります:

```bash
sudo apt-get install -y libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libx11-dev libgl1-mesa-dev
```

### 4-2. all-in-one で使う (既定)

サーバー機能を同プロセスに内包し、**TCP ポートを一切開かない**。
同じ端末のブラウザからも、他の端末からもアクセスできない。

```bash
./target/release/illumia-desktop
```

データディレクトリを指定する場合:

```bash
ILLUMIA_DATA_DIR=~/Illumia ./target/release/illumia-desktop
```

未指定なら OS 標準のアプリデータ位置を使う
(macOS: `~/Library/Application Support/com.shiningwank0.Illumia`)。

### 4-3. client-only で使う (リモートサーバーへ接続)

```bash
ILLUMIA_DESKTOP_MODE=client-only \
ILLUMIA_SERVER_URL=https://illumia.example.com \
./target/release/illumia-desktop
```

初回は terminal で password を echo 無しで入力する。発行された device token と
`instance_id` は macOS Keychain / Windows Credential Manager に保存され、以後は
password の再入力なしで利用できる。token を環境変数やコマンドラインへ書かない。

`ILLUMIA_SERVER_URL` は HTTPS を推奨する。平文 HTTP は loopback のみ指定できるが、起動の
たびに警告への明示確認が必要で、Bearer token を送る前に unauthenticated
`/api/server/info` の identity を secure storage の pin と照合する。初回 TOFU の pin も
利用者が表示値を確認して承認するまで password を送らない。

旧手順で `ILLUMIA_DEVICE_TOKEN` を shell に直接書いたことがある場合は、shell history から
削除したうえで device 管理 API (`GET/DELETE /api/auth/devices`) から古い desktop token を
失効し、新しい interactive login に移行する。

### 4-4. M6 v1 の制限

デスクトップ版はタイムライン (日/月/年の粒度切替・justified タイル) と
ビューアまで。漫画スタック・Vault・検索・人物クラスタの画面、自動アップロード、
Touch ID は未実装で、Web / Android 版を使ってください (→ docs/08, docs/10)。

Windows では ML サイドカーが利用できません (named pipe 実装が未了。
TCP へのフォールバックは要件上行いません)。

---

## 5. 困ったときは

| 症状 | 確認すること |
|---|---|
| ブラウザで真っ白 | サーバーのログと、ブラウザのコンソールに CSP エラーが無いか |
| Android で「サーバーに到達できません」 | URL が `https` か、外部から到達できるか、証明書が有効か |
| Android で「別のサーバーが応答しました」 | 接続先が初回登録時と別のサーバー。偽サーバーの可能性があるので、信頼できないネットワークでは接続しない |
| `docker compose pull` が 401 | GHCR に `read:packages` PAT でログインしているか |
| APK が更新できない | 以前と同じ署名鍵か。違う場合は一度アンインストールが必要 |
