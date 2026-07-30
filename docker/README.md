# Illumia の Docker 配布

> **配布ポリシー (重要)**: `ghcr.io/shiningwank0/illumia-server` は private リポジトリに
> 紐づく **private パッケージ**として運用する。GitHub のパッケージ設定で
> **絶対に Visibility を public に変更しないこと** (変更すると全世界に公開される)。
> 共有したい相手には、(1) リポジトリの collaborator に招待する、または
> (2) `read:packages` のみの Fine-grained PAT を発行して渡す。受け取った側は
> `echo <PAT> | docker login ghcr.io -u <github-user> --password-stdin` で pull できる。

Illumia のサーバーと Web UI を1つのコンテナで実行します。コンテナは TCP
`0.0.0.0:2283` で待ち受け、Web UI も同じポートから配信します。永続データはすべて
コンテナ内の `/data` に保存されます。

現時点の Compose 構成では `illumia-server` のみを起動します。設計上の
`illumia-ml` サイドカーは将来追加予定です。

## Docker Compose で起動する

リポジトリのルートで次を実行します。

```sh
docker compose -f docker/compose.yaml pull
docker compose -f docker/compose.yaml up -d
```

公開済みの `ghcr.io/shiningwank0/illumia-server:latest` イメージを使用します。
状態とログは次のコマンドで確認できます。

```sh
docker compose -f docker/compose.yaml ps
docker compose -f docker/compose.yaml logs -f illumia-server
```

停止する場合は次を実行します。named volume は削除されないため、データは保持されます。

```sh
docker compose -f docker/compose.yaml down
```

`down -v` は永続データの volume も削除するため、データを失ってよい場合を除いて
実行しないでください。

## 初回セットアップ

1. コンテナの起動後、ブラウザで `http://<サーバーのIPアドレス>:2283` を開きます。
2. 初回セットアップ画面で管理用パスワードと、このブラウザを識別する端末名を設定します。
3. セットアップ後は同じ URL からログインして利用します。

外部ネットワークから公開する場合は、直接 2283 番ポートをインターネットへ開放せず、
認証と TLS を設定したリバースプロキシや VPN の利用を推奨します。

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
設定してください。

## TrueNAS SCALE の Custom App

先に Illumia 用 dataset（例: `/mnt/pool/illumia`）を作成し、UID/GID `1000:1000`
から読み書きできる権限を設定します。その後、TrueNAS の Apps 画面から Custom App を
追加し、次の値を設定します。

- イメージ: `ghcr.io/shiningwank0/illumia-server:latest`
- コンテナポート／ホストポート: `2283` / `2283`
- ホストパス: `/mnt/pool/illumia`
- コンテナ内パス: `/data`
- 再起動ポリシー: `unless-stopped`
- 環境変数: 下表の3項目

TrueNAS SCALE の「Install via YAML」を使う場合は、`compose.yaml` を基にしつつ、
ローカルソースを必要とする `build:` セクションを削除し、`illumia_data:/data` を
`/mnt/pool/illumia:/data` に置き換えて登録します。画面名や入力欄は TrueNAS の
バージョンによって異なるため、利用中のバージョンの Custom App ドキュメントも確認して
ください。

## 環境変数

| 変数 | 既定値 | 説明 |
|---|---|---|
| `ILLUMIA_DATA_DIR` | `/data` | DB、画像、Vault などを保存するデータルート。コンテナでは `/data` のまま使用します。 |
| `ILLUMIA_ADDR` | `0.0.0.0:2283` | HTTP サーバーの listen アドレス。コンテナ外から接続するため `0.0.0.0` を使用します。 |
| `ILLUMIA_WEB_DIST` | `/app/web` | サーバーが配信する Web SPA のビルド成果物ディレクトリ。 |

これらは `docker/Dockerfile.server` に設定済みです。通常は上書き不要です。

## `docker run` で起動する

Compose を使わない場合は named volume を作成して次のように起動できます。

```sh
docker volume create illumia_data
docker run -d \
  --name illumia-server \
  --restart unless-stopped \
  -p 2283:2283 \
  -v illumia_data:/data \
  ghcr.io/shiningwank0/illumia-server:latest
```

リポジトリ内の Dockerfile は GitHub Actions で本番イメージを作るためのものです。
ローカルビルドを行う場合は動作確認用途に限定し、ローカル成果物を配布しないでください。
