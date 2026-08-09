# Android APK 署名キーの作成と登録

APK は GitHub Actions でunsigned buildと署名を別jobに分ける
(→ `.github/workflows/release.yml`)。
署名キーと そのパスワードは**あなただけが保持する秘密**なので、以下は必ず
あなた自身の端末で実行すること。生成物と入力値を第三者 (AI エージェントを含む) へ
渡さないこと。

> **一度作ったキーは絶対に失くさないこと。**
> Android は「同じ署名キーで署名された APK」しか上書きインストールを許さない。
> キーを失うと、既存ユーザーは一度アンインストールしないと更新できなくなる。
> keystore ファイルとパスワードは、パスワードマネージャ + オフラインバックアップの
> 2 系統で保管する。

## 1. keystore を作る

`keytool` は JDK に付属する (Android Studio か `brew install openjdk` で入る)。

**注意**: 専用署名jobはストアパスワードをキーパスワードにも使用するため、
`-storepass` と `-keypass` は同一にすること。

```bash
keytool -genkeypair -v \
  -keystore illumia-release.keystore \
  -alias illumia \
  -keyalg RSA -keysize 4096 -validity 10000 \
  -dname "CN=Illumia, OU=Illumia, O=Illumia, L=, ST=, C=JP"
```

実行すると `Enter keystore password:` と聞かれるので、強いパスワードを入力する
(以後 `keypass` も同じ値にする。プロンプトで「キーのパスワードをストアと同じにするか」
を聞かれたら Enter でそのまま同一にできる)。

`-validity 10000` は約 27 年。有効期限が切れると新しい APK を配布できなくなるため、
短くしないこと。

## 2. base64 化する

GitHub Secrets はテキストしか保持できないので、keystore を base64 にする。

```bash
base64 -i illumia-release.keystore | tr -d '\n' > illumia-keystore.b64
```

## 3. GitHub Secrets に登録する

リポジトリの **Settings → Secrets and variables → Actions → New repository secret**
から、次の 3 つを登録する。`gh` CLI を使う場合は下のコマンドでもよい
(パスワードは対話入力になる)。

| Secret 名 | 値 |
|---|---|
| `ILLUMIA_ANDROID_KEYSTORE_B64` | 手順 2 で作った `illumia-keystore.b64` の中身 |
| `ILLUMIA_ANDROID_KEYSTORE_PASSWORD` | 手順 1 で決めたパスワード |
| `ILLUMIA_ANDROID_KEY_ALIAS` | `illumia` (手順 1 の `-alias` と同じ値) |

```bash
gh secret set ILLUMIA_ANDROID_KEYSTORE_B64 < illumia-keystore.b64
gh secret set ILLUMIA_ANDROID_KEYSTORE_PASSWORD   # 入力を求められる
gh secret set ILLUMIA_ANDROID_KEY_ALIAS --body illumia
```

## 4. 登録できたか確認する

```bash
gh secret list
```

3 つ並べば完了。`base64` の中間ファイルは不要なので消す。

```bash
rm illumia-keystore.b64
```

`illumia-release.keystore` 本体は**消さずに**安全な場所へ退避する
(リポジトリには絶対に置かない。`.gitignore` 済みだが、そもそも作業ディレクトリに
残さないこと)。

## 5. リリース

Secrets が揃った状態で `vX.Y.Z` タグを push すると、release workflow が
依存buildを秘密なしで完了した後、repositoryをcheckoutしない別jobでAPKを署名する。
署名jobはSHA-256と署名証明書のfingerprintを検証・記録し、unsigned APKはReleaseへ
添付しない (docs/12: SEC-007)。

タグを打つ前に、公開しない経路で全ジョブが通ることを確認できる:

```bash
gh workflow run release.yml --ref main -f dry_run=true
```

## 配布後に fingerprint を確認する

利用者が「配布された APK が本物か」を確かめられるよう、release notes の
fingerprint と手元の APK を突き合わせられる。

```bash
apksigner verify --print-certs illumia.apk
sha256sum illumia.apk
```
