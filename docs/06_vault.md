# 06. 非表示フォルダ (Vault)

Immich の Locked Folder 相当 + 独自要件。vault に入れたものは
タイムライン・検索・クラスタ表示から完全に消え、**閲覧のたびにパスワード
(または生体認証) が必要**。Immich と異なり **vault 内でもダウンロード可**。

## 脅威モデル (前提を明示)

- 守るもの: **at-rest のデータ** (NAS のディスク・バックアップ・DB ダンプを直接見られても
  vault の内容・存在痕跡が分からないこと)。
- 守らないもの: アンロック中のサーバープロセスメモリ。サーバー管理者 = ユーザー本人という
  シングルユーザー前提であり、アンロック中はサーバー RAM に鍵が存在する。
- 平文側 DB・ログ・ジョブ履歴に vault 内のファイル名・ID・件数以上の情報を残さない。

## 鍵設計

```
password ──Argon2id──► KEK (32B)         # salt は vault.keyfile に保存
                        │
vault.keyfile: XChaCha20-Poly1305_wrap(KEK, MK)   # MK = 初期化時に CSPRNG で生成 (32B)
                        │
                        ├─ HKDF(MK, "vault-db")   → SQLCipher キー (vault.db)
                        └─ file_key (per-file, CSPRNG 32B)
                           vault.db に XChaCha20-Poly1305_wrap(MK, file_key) で保存
```

- Argon2id パラメータ: m=64MiB, t=3, p=1 (settings で強化可。keyfile にパラメータを記録)。
- パスワード変更 = KEK 再導出して MK を再ラップするだけ。ファイル再暗号化は不要。
- **パスワードを忘れたら復元不能** (リカバリーキーを初期化時に 1 度だけ表示し、
  ユーザー保管とする。リカバリーキーでも MK をラップした別レコードを keyfile に持つ)。

## ファイル暗号化フォーマット

- 方式: XChaCha20-Poly1305 の **チャンク AEAD (secretstream 方式)**。チャンク 1MiB。
  大きな画像でも全読み込みせずストリーム復号できる。
- blob ファイル: `vault/blobs/<uuid>` (拡張子なし・ランダム名)。
  ヘッダ: magic `ILMV1` + nonce prefix + chunk size。AAD に blob uuid を入れる
  (ファイル差し替え検知)。
- 原本・240px サムネ・1440px プレビューをそれぞれ別 blob として暗号化保存。
- 平文の一時ファイルをディスクに書かないこと (メモリ内で暗号化してから書く。
  ストリーミング時も同様)。

## アンロックセッション

- `POST /api/vault/unlock {password}` → 成功時 `{vault_session, expires_at}`。
  MK はサーバープロセスのメモリのみに保持 (zeroize 対応の型を使う)。
- セッション TTL: 既定 15 分、操作で延長 (settings で変更可)。`lock` / TTL 切れ /
  プロセス終了で MK を破棄。
- ロック中の vault 系 API は一律 **404** (存在秘匿。401 だと「vault がある」ことが分かる)。
- 生体認証 (→ docs/08): 端末の Keystore/Keychain に「ラップ済み MK」を保存し、
  生体認証成功でクライアントが unlock ペイロードを組み立てる方式。
  サーバー側は unlock API の別バリアント `{wrapped_mk_proof}` を受ける。

## vault 内でできること (要件)

- 閲覧 (タイムライン相当のビュー) / **原本ダウンロード (許可する。Immich との差別化点)**
- 検索 (vault.db 内の FTS。平文側インデックスには一切入れない)
- 漫画スタックの作成・編集・リーダー (vault.db 内で完結)
- スタック丸ごとの vault 移動 / vault 内画像からのスタック作成 (→ docs/05)
- キャラクタークラスタ表示・命名 (vault.db 内で完結)

## ML 処理とログ抑制 (要件)

- vault の ML 解析 (検出・埋め込み・クラスタリング) は**アンロック中のみ**実行。
  サーバーが blob を復号し、画像バイトをサイドカーへ渡す (unix socket 経由)。
  結果は vault.db にのみ書く。サイドカーは何も保存・キャッシュしない (→ docs/07)。
- vault ジョブは vault.db 内の jobs テーブルで管理。平文側 jobs には入れない。
- ログ・進捗表示・WS 通知は汎用文言のみ: 「vault ジョブ N 件処理中」。
  ファイル名・asset id・クラスタ名等を stdout / ログファイル / 平文 DB に出さないこと。
  実装時は vault パスを扱う関数に `#[doc = "vault: no-log"]` 相当の規約コメントを付け、
  レビュー観点にする (→ docs/09)。

## 出し入れ (メイン ⇄ vault)

`POST /api/vault/import {asset_ids}` (メイン → vault):
1. vault.db に asset 行 + 関連 (faces/embeddings/stack 構造) を複製
2. 原本・サムネを暗号化して blobs へ書き込み
3. 平文側から DB 行 (faces / stack_pages / FTS を含む)・原本・サムネを**完全削除**
4. 1-3 は「vault 書き込み成功を確認してから平文削除」の順。途中失敗時は vault 側をロールバック

export は逆方向 (復号 → 平文側へ復元 → vault 側から完全削除)。
移動後に平文側へ痕跡 (DB 行・ファイル・FTS・WAL 内の残骸) が残らないことをテストで検証する。
WAL は import 後に `PRAGMA wal_checkpoint(TRUNCATE)` を実行して切り詰める。

## テスト要件

- 移動後の平文側痕跡ゼロ (DB 全テーブル走査 + ファイルシステム走査 + FTS 検索)
- ロック中: vault API 全てが 404 / vault blob が AEAD なしで読めない
- パスワード変更後も既存 blob が読める / 旧パスワードで unlock 不可
- クラッシュ注入 (import の各段階で kill) してもメイン・vault いずれかに必ず完全な状態が残る
