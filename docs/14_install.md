# 14. インストールと使い方 (プラットフォーム別)

Illumia は「サーバー 1 台 + 各端末のクライアント」という構成で使う。
まずサーバーを立て、そのあとで各端末から接続する。

| 形態 | 役割 | 配布物 |
|---|---|---|
| Docker (TrueNAS 等) | **サーバー** | GHCR の private イメージ |
| Web ブラウザ | クライアント | サーバーが配信 (追加インストール不要) |
| Android | クライアント | GitHub Release の APK (サイドロード) |
| macOS / Windows | クライアント / all-in-one | 現状は自前ビルド (下記参照) |

> **リポジトリと GHCR イメージは private です。** 配布物を受け取るには
> collaborator 招待か、`read:packages` 権限の Personal Access Token (PAT) が要ります。

---

## 1. サーバー (Docker)

TrueNAS 等の常時稼働機に立てる。詳細な運用手順は
[docker/README.md](../docker/README.md)、セキュリティ要件は
[docs/12_security.md](12_security.md) を参照。

### 1-1. GHCR にログインする

イメージは private なので、まず認証する。GitHub の
Settings → Developer settings → Personal access tokens で
`read:packages` 権限の PAT を作る。

```bash
echo "<あなたのPAT>" | docker login ghcr.io -u <GitHubユーザー名> --password-stdin
```

### 1-2. compose ファイルと .env を用意する

```bash
git clone https://github.com/ShiningWank0/Illumia.git
cd Illumia/docker
cp .env.example .env
```

`.env` を編集し、`ILLUMIA_SETUP_TOKEN` に 32 文字以上のランダム値を設定する
(初回セットアップの横取りを防ぐため → docs/12)。

```bash
# 十分にランダムな setup token を生成する例
openssl rand -hex 32
```

**権限を必ず絞る** (docs/12 が要求。世界読み取り可のままにしない):

```bash
chmod 600 .env
```

### 1-3. 起動する

```bash
docker compose --profile ml up -d
```

ML (キャラクター認識) を使わない場合は `--profile ml` を外す。

- ホストポートは既定で `127.0.0.1` のみに bind する。
  インターネット公開は必ず Pangolin/Newt 等のリバースプロキシ経由にすること。
- 初回セットアップはリバースプロキシで公開する**前**に済ませることを推奨。

### 1-4. 初期セットアップ

ブラウザで `http://127.0.0.1:2283` を開き、パスワードを設定する。
`ILLUMIA_SETUP_TOKEN` を設定した場合は、その値も入力する。

セットアップ完了後は `.env` から setup token を削除するか rotate し、
コンテナを再作成する。

### 1-5. 更新する

```bash
docker compose pull
docker compose --profile ml up -d
```

再現性を重視するなら、`latest` ではなく Release notes に載っている
**digest 指定**を使う (例: `ghcr.io/shiningwank0/illumia-server@sha256:...`)。

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

リポジトリが private なので、**ブラウザでダウンロードするには GitHub に
ログインしている必要がある**。

**方法 A: ブラウザ (かんたん)**

1. Android 端末の Chrome で GitHub にログインする。
2. <https://github.com/ShiningWank0/Illumia/releases/latest> を開く。
3. `Assets` を展開し、`app-universal-release.apk` をタップする。
4. 「この種類のファイルは端末に損害を与える可能性があります」と出るが、
   自分でビルドした APK なので `OK` / `ダウンロード` を選ぶ。

**方法 B: gh CLI (PC でダウンロードして転送)**

```bash
gh release download v0.2.0 --repo ShiningWank0/Illumia --pattern "*.apk"
```

**方法 C: curl (gh CLI が使えない環境)**

`repo` 権限の PAT が必要。private リポジトリのアセットは、ブラウザ用 URL では
なく **API の asset id** に対して `Accept: application/octet-stream` で
取得する。

```bash
TOKEN=<あなたのPAT>; REPO=ShiningWank0/Illumia; TAG=v0.2.0
ID=$(curl -sH "Authorization: Bearer $TOKEN" \
  "https://api.github.com/repos/$REPO/releases/tags/$TAG" \
  | python3 -c "import sys,json;print([a['id'] for a in json.load(sys.stdin)['assets'] if a['name'].endswith('.apk')][0])")
curl -L -o app-universal-release.apk \
  -H "Authorization: Bearer $TOKEN" -H "Accept: application/octet-stream" \
  "https://api.github.com/repos/$REPO/releases/assets/$ID"
```

> 素のブラウザ用 URL を `curl` で叩くと、APK ではなくログインページの HTML が
> 落ちてくる。ダウンロード後は必ず 3-2 のハッシュ照合をすること。

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
- 生体認証による Vault アンロック
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
ILLUMIA_DEVICE_TOKEN=<device token> \
./target/release/illumia-desktop
```

`ILLUMIA_SERVER_URL` は `https` のみ (平文はループバックのみ許可)。

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
