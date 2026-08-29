# Illumia の Docker 配布

> **配布ポリシー (重要)**: リポジトリ、GitHub Release、
> `ghcr.io/shiningwank0/illumia-server` / `illumia-ml` package は通常 **private** とする。
> GitHub Actionsの実行枠を確保するためリポジトリを一時的にpublicへ切り替えることはあるが、
> GitHub Releaseはその期間中に公開してよい。公開repository上のReleaseは第三者から閲覧可能に
> なることを理解した上で実行し、CI/CD作業後はrepositoryをprivateへ戻す。GHCR packageの
> Visibilityはpublicへ変更せず、配布先はcollaboratorへ限定するか、`read:packages` だけを持つ
> tokenで認証し、release notesに記録したimmutable digestをpullする。

Illumia のサーバーと Web UI を1つのコンテナで実行します。コンテナは TCP
`0.0.0.0:2283` で待ち受けますが、Compose が host へ publish するのは既定で
`127.0.0.1:2283` だけです。Web UI も同じポートから配信し、永続データはすべて
コンテナ内の `/data` に保存します。

既定では `illumia-server` のみを起動します。`illumia-ml` サイドカーはモデル未配置時も
mock で動作しますが、リソース節約のため `ml` profile による opt-in です。

## Docker Compose で起動する

リポジトリのルートで次を実行します。

```sh
cp docker/.env.example docker/.env
chmod 600 docker/.env
# docker/.env の ILLUMIA_SETUP_TOKEN に `openssl rand -hex 32` の出力を設定する
# Release notes の server/ML image@sha256 digest も設定する
./docker/compose-prod.sh pull
./docker/compose-prod.sh up -d
```

`.env` の `ILLUMIA_SERVER_DIGEST` / `ILLUMIA_ML_DIGEST` を、YAML 内に固定した
`ghcr.io/...@sha256:` へ連結した immutable reference だけを使用します。
digest 未設定時や `.env.example` の zero digest のままでは安全側に pull が失敗します。
状態とログは次のコマンドで確認できます。

```sh
./docker/compose-prod.sh ps
./docker/compose-prod.sh logs -f illumia-server
```

停止する場合は次を実行します。named volume は削除されないため、データは保持されます。

```sh
./docker/compose-prod.sh down
```

`down -v` は永続データの volume も削除するため、データを失ってよい場合を除いて
実行しないでください。

## ML サイドカーを有効にする

ML サイドカーを含めて pull・起動する場合は `ml` profile を指定します。

```sh
./docker/compose-prod.sh --profile ml pull
./docker/compose-prod.sh --profile ml up -d
```

モデルバンドルは application data と分離した `illumia_models` named volume、コンテナから見て
`/models/<bundle_name>/` に配置します。サイドカーからは読み取り専用でマウントされ、
main DB/library/Vault を含む `illumia_data` は一切 mount されません。探索先は
`ILLUMIA_MODEL_DIR=/models` です。必要なファイル構成と checksum の要件は
[モデル要件](../docs/13_model_requirements.md) を参照してください。

配置後に `shasum -a 256 checksums.sha256` の値を `.env` の
`ILLUMIA_TRUSTED_MODEL_DIGESTS` へ設定します。bundle 内 checksum だけでは真正性を保証しないため、
この外部 pin が無い場合は mock backend へ fail closed します。

起動後、認証済み device token を用いて設定 API に共有 UDS のパスを登録します。
`ILLUMIA_DEVICE_TOKEN` は `POST /api/auth/login` で取得した token を設定してください。

```sh
curl --fail-with-body -X PATCH http://127.0.0.1:2283/api/settings \
  -H "Authorization: Bearer ${ILLUMIA_DEVICE_TOKEN}" \
  -H "Content-Type: application/json" \
  --data '{"ml.socket_path":"/run/illumia/ml.sock","ml.enabled":true}'
```

`illumia-server` と `illumia-ml` は `illumia_sock` named volume 上の Unix domain socket
だけで通信します。ML コンテナは `network_mode: none` のため TCP/IP ネットワークへ
接続しません。モデル未配置時は mock バックエンドへフォールバックします。

## 初回セットアップ

1. `docker/.env` に十分にランダムな `ILLUMIA_SETUP_TOKEN` を設定して起動します。
2. 同一hostのブラウザ、SSH port forwarding、または認証済みPangolin経由で
   `http://127.0.0.1:2283` 相当へアクセスします。
3. 初回画面でsetup token、Illumiaの共有パスワード、このブラウザを識別する端末名を
   入力します。Illumia自身にログインIDはなく、端末名は認証要素ではありません。
4. 完了後は `.env` からsetup tokenを削除してコンテナを再作成します。

初回セットアップ前にsetup tokenなしで非loopback listenしようとすると、serverは
fail closedで終了します。LANから平文HTTPで直接使うためhost bindを広げる運用は推奨しません。
どうしても行う場合だけComposeのport bindと `ILLUMIA_SECURE_COOKIES=false` を明示的に
変更し、router/firewallでLAN外を拒否してください。

## Pangolin / Newt で公開する

- Newtをhost上で動かす場合、resourceのtargetを `http://127.0.0.1:2283` にします。
- Newtもcontainerの場合はIllumiaと専用のprivate Docker networkを共有し、Illumiaの
  `ports:` を削除してtargetを `http://illumia-server:2283` にします。
- Pangolin resource authenticationを必ず有効にし、Illumiaとは別の強いpasswordとMFAを
  使います。日本限定ruleは `country=JP` を **Pass to Auth**、末尾を **Deny** にします。
  `Allow` はPangolin認証を迂回するため使いません。
- GeoIPはVPN、国内proxy、国内botを防げません。CrowdSecも別途導入・bouncer・logを設定
  した場合だけ機能します。Newtを起動しただけで怪しい通信が自動blockされるとは扱いません。
- routerのport forwarding、UPnP、IPv6 firewallを確認し、外部回線からorigin IPの2283へ
  直接到達できないことを実測します。外部公開はHTTPSのみとします。

## データの永続化

`compose.yaml` は named volume `illumia_data` を `/data` にマウントします。ここには
SQLite データベース、画像、サムネイル、Vault、モデルキャッシュなど、サーバー側の
永続状態が保存されます。コンテナを更新・再作成しても、この volume を残す限りデータは
保持されます。

ホスト側の保存先を明示する場合は、`compose.yaml` の volume を次のような
バインドマウントに置き換えます。

```yaml
volumes:
  - /mnt/pool/illumia:/data
```

コンテナは UID/GID `1000:1000` の非 root ユーザー `illumia` で動作します。バインド
マウントを使う場合は、保存先ディレクトリをこの UID/GID から読み書きできるように
設定してください。serverはUnix上でdata rootを `0700`、DB/keyfileを `0600` にします。
ACLやfilesystemの制約でこの権限を設定できない保存先では起動に失敗します。

## TrueNAS SCALE の Custom App

先に Illumia 用 dataset（例: `/mnt/pool/illumia`）を作成し、UID/GID `1000:1000`
から読み書きできる権限を設定します。その後、TrueNAS の Apps 画面から Custom App を
追加し、次の値を設定します。

- イメージ: Release notes の `ghcr.io/shiningwank0/illumia-server@sha256:<digest>`
- コンテナポート: `2283`。host portは原則publishせず、必要ならloopbackだけ
- ホストパス: `/mnt/pool/illumia`
- コンテナ内パス: `/data`
- 再起動ポリシー: `unless-stopped`
- 環境変数: 下表。初回のみ `ILLUMIA_SETUP_TOKEN` も必須

TrueNAS SCALE の「Install via YAML」を使う場合は、build を含まない production 用
`compose.yaml` を基にしつつ、`illumia_data:/data` を
`/mnt/pool/illumia:/data` に置き換えて登録します。画面名や入力欄は TrueNAS の
バージョンによって異なるため、利用中のバージョンの Custom App ドキュメントも確認して
ください。

## 環境変数

| 変数 | 既定値 | 説明 |
|---|---|---|
| `ILLUMIA_DATA_DIR` | `/data` | DB、画像、Vault などを保存するデータルート。コンテナでは `/data` のまま使用します。 |
| `ILLUMIA_ADDR` | `0.0.0.0:2283` | HTTP サーバーの listen アドレス。コンテナ外から接続するため `0.0.0.0` を使用します。 |
| `ILLUMIA_WEB_DIST` | `/app/web` | サーバーが配信する Web SPA のビルド成果物ディレクトリ。 |
| `ILLUMIA_SETUP_TOKEN` | 空 | 初回セットアップ用の32〜256 byte secret。初回完了後は削除。 |
| `ILLUMIA_SECURE_COOKIES` | `true` | HTTPS公開時は必ずtrue。平文LAN直結時だけfalse。 |
| `ILLUMIA_TRUSTED_PROXY_CIDRS` | 未設定 | client forwarding headerを除去・再設定するproxy peerだけをCIDR（comma区切り）で指定。未指定peerのheaderは無視する。 |

先頭3項目は `docker/Dockerfile.server` に設定済みです。secretをCompose YAML、Git、
shell history、通常logへ直接書かないでください。

## `docker run` で起動する

Compose を使わない場合は named volume を作成して次のように起動できます。

```sh
docker volume create illumia_data
docker run -d \
  --name illumia-server \
  --restart unless-stopped \
  -p 127.0.0.1:2283:2283 \
  --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m \
  --cap-drop ALL \
  --security-opt no-new-privileges=true \
  --pids-limit 256 \
  --memory 3g \
  --cpus 4 \
  --env-file docker/.env \
  -v illumia_data:/data \
  "ghcr.io/shiningwank0/illumia-server@sha256:${ILLUMIA_SERVER_DIGEST}"
```

`ILLUMIA_SERVER_DIGEST` は Release notes の server image の64桁 digest 本体を指定してください。
mutable tag と `latest` は production では使用しません。

リポジトリ内の Dockerfile は GitHub Actions で本番イメージを作るためのものです。
ローカルビルドを行う場合は動作確認用途に限定し、ローカル成果物を配布しないでください。
明示的な開発ビルドは `.env.example` を使う次の overlay にだけ存在します。

```sh
docker compose --env-file docker/.env.example \
  -f docker/compose.yaml -f docker/compose.dev.yaml --profile ml build
```
