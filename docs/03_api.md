# 03. API 仕様 (REST + WebSocket)

illumia-server が公開する HTTP API。all-in-one 版では同じサービス層を
in-process 呼び出しするため、**ハンドラにロジックを書かないこと** (→ docs/01)。

- ベースパス: `/api`
- 認証:
  - ネイティブクライアントは `Authorization: Bearer <device_token>`。
  - 同一オリジンの Web SPA は `HttpOnly; SameSite=Strict` Cookie。JavaScript から
    device token を受け取らず、永続ストレージへも保存しない。setup / login 時は
    `X-Illumia-Auth-Mode: cookie` を送り、token を response body に含めない。
  - 初回セットアップとログインを除く。ただし非 loopback での初回セットアップには
    `X-Illumia-Setup-Token` が必須 (→ docs/12)。
- Vault 系は追加で `X-Vault-Session: <vault_session_token>` (→ docs/06)
- Cookie 認証で状態を変更するリクエストは、`Origin` の authority が `Host` と一致しない
  場合、または `Origin` が欠落する場合に拒否する。Bearer 認証のネイティブクライアントは
  この CSRF 検査の対象外。
- エラー: `{ "error": { "code": "string", "message": "string" } }`。
  Vault ロック中の vault 系エンドポイントは一律 **404** を返す (存在秘匿)。

## 認証

| Method | Path | 内容 |
|---|---|---|
| GET  | /api/server/info | セットアップ済みか・現在の認証状態・setup token 要否 (未認証可)。正確な version は認証後のみ |
| POST | /api/auth/setup | 初回のみ。パスワード設定 → token 発行。構成により setup token 必須 |
| POST | /api/auth/login | `{password, device_name}` → `{token}` |
| POST | /api/auth/logout | 現在の device token を失効し、認証 Cookie を削除 |
| GET  | /api/auth/devices | 発行済みデバイストークン一覧 |
| DELETE | /api/auth/devices/{id} | トークン失効 |

シングルユーザーであり、**ログイン ID / ユーザー名は存在しない**。`device_name` は
発行済み token を識別する表示ラベルであって認証要素ではない。パスワードは Argon2id
でハッシュ化して settings に保存し、新規設定時は 12 文字以上を要求する。
トークンは 256bit ランダム、DB には SHA-256 のみ保存する。発行数は 256 を上限とする。
認証成功レスポンスは、ネイティブクライアントには token を返す。Web から
`X-Illumia-Auth-Mode: cookie` が指定された場合は `204` と Cookie だけを返し、
JavaScript / Network response body に token を露出しない。Cookie の Path は `/api`。
API response は既定で `Cache-Control: private, no-store` とし、認証済みのサムネイルと
プレビューだけ `private, max-age=31536000, immutable` を許可する。

ログインと初期セットアップは、送信元ごとの失敗回数制限と Argon2 の同時実行数制限を
適用する。パスワード・device name・setup token には処理前の長さ上限を設ける。
保存済み Argon2 PHC の memory / iteration / parallelism / output 長も検証してから計算する。

## アセット

| Method | Path | 内容 |
|---|---|---|
| POST | /api/assets | multipart upload。`X-Illumia-Checksum: <blake3 hex>` 必須 |
| POST | /api/assets/exists | `{hashes: [hex]}` → `{exists: {hex: asset_id}}` (自動アップロードの事前照合) |
| GET  | /api/assets/{id} | メタデータ |
| GET  | /api/assets/{id}/original | 原本。`Content-Disposition` 付与 (ダウンロード) |
| GET  | /api/assets/{id}/thumbnail | 240px WebP。`Cache-Control: immutable` |
| GET  | /api/assets/{id}/preview | 1440px WebP |
| DELETE | /api/assets/{id} | ゴミ箱へ (物理削除ではない → docs/11) |
| POST | /api/assets/{id}/restore | ゴミ箱から復元 |

- upload レスポンス: `201 {id, status: "created"}` または
  `200 {id, status: "duplicate", duplicate_of}` (重複でも保存される → docs/11)。
- checksum 不一致は 400。サーバー側で BLAKE3 を再計算して検証する。
- multipart 全体は 129 MiB、ファイル本体は 128 MiB を上限とする。画像 decoder には
  対応 format を明示し、幅・高さ各 32768 px、decode allocation 512 MiB を上限とする。
  拡張子と実データの format が一致しない入力は拒否する。
- v1 の入力 format は JPEG / PNG / WebP / GIF のみ。AVIF は既存実装が decoder を
  持たず、native decoder の supply-chain / cross-build 条件も別途評価が必要なため、
  対応を明示的に追加するまでは 400 で拒否する。

## タイムライン (→ docs/04)

| Method | Path | 内容 |
|---|---|---|
| GET | /api/timeline/buckets?granularity=day\|month\|year | `[{key, count}]` |
| GET | /api/timeline/buckets/{key}?granularity=... | `[{id, ratio, thumbhash, taken_at}]` |

- bucket key: day=`YYYY-MM-DD`, month=`YYYY-MM`, year=`YYYY` (taken_at_local_date 基準)。
- レスポンスは `visible_in_timeline = 1` のみ。`taken_at DESC` 順。
- ETag 対応 (bucket の max(updated) ベース) でクライアントキャッシュを効かせる。

## ゴミ箱・重複 (→ docs/11)

| Method | Path | 内容 |
|---|---|---|
| GET | /api/trash | ゴミ箱一覧 (`purge_after` 付き) |
| GET | /api/duplicates | 重複一覧。`[{dup: {...asset}, original: {...asset}, purge_after}]` |
| DELETE | /api/trash/{id} | 即時パージ (ユーザー明示操作のみ) |

## 漫画スタック (→ docs/05)

| Method | Path | 内容 |
|---|---|---|
| POST | /api/stacks | `{title, asset_ids[]}` → 章1つ+ページを作成 |
| GET | /api/stacks | 一覧 (cover・話数・ページ数) |
| GET | /api/stacks/{id} | 章・ページの完全な構造 |
| PATCH | /api/stacks/{id} | title / cover 変更 |
| DELETE | /api/stacks/{id} | スタック解散 (画像は削除しない) |
| PUT | /api/stacks/{id}/structure | 章構成+ページ順の一括置換 (→ docs/05 §並べ替え) |
| POST | /api/stacks/{id}/pages | `{asset_ids[], chapter_id?}` 追加 (重複ビューからの追加を含む) |
| DELETE | /api/stacks/{id}/pages/{asset_id} | スタックから外す |
| PATCH | /api/stacks/{id}/pages/{asset_id} | `{show_in_timeline: bool}` |

## 検索

| Method | Path | 内容 |
|---|---|---|
| GET | /api/search?q=... | ファイル名・クラスタ名・スタック名の横断検索 |

- FTS5 trigram により**日本語の部分一致**が動作すること (「らき☆すた」「主人公」等)。
  2 文字以下のクエリは LIKE フォールバック。
- `q` は 256 文字を上限とし、LIKE の `%` / `_` / `\` は文字として escape する。
  全検索結果には固定の件数上限を設ける。値は常に bind parameter とし、SQL 文字列へ
  連結しない。
- レスポンス: `{assets: [...], stacks: [...], clusters: [...]}`。
- 将来のスマートサーチ/OCR/タグ検索は `q` の解釈を拡張する形で同エンドポイントに載せる
  (→ docs/10)。

## キャラクター (クラスタ) (→ docs/07)

| Method | Path | 内容 |
|---|---|---|
| GET | /api/clusters | 一覧 (名前・代表顔・枚数) |
| GET | /api/clusters/{id}/assets | クラスタ内アセット |
| PATCH | /api/clusters/{id} | `{name}` 命名 / 改名 |
| POST | /api/clusters/merge | `{from_id, into_id}` |
| POST | /api/clusters/{id}/split | `{face_ids[]}` を新クラスタへ |
| GET | /api/review/candidates | 確認キュー (candidate 状態の顔) |
| POST | /api/review/candidates/{face_id} | `{action: "accept"\|"reject"}` |

- `GET /api/clusters` は
  `[{id, name, cover: {face_id, asset_id, bbox}|null, asset_count}]` を返す。
  `bbox` は正規化座標 `[x, y, w, h]`。`cover_face_id` に対応する face が存在しない、
  または対応 asset がゴミ箱内の場合は `cover` を `null` とする。
- `GET /api/clusters/{id}/assets` は AssetResponse の各要素に
  `faces: [{face_id, bbox, state, similarity}]` を追加して返す。`faces` には指定した
  クラスタに属する face だけを含める。同じ asset に複数の該当 face がある場合も
  すべて返す。
- vault ミラーの `/api/vault/clusters` と `/api/vault/clusters/{id}/assets` も同じ
  レスポンス形状を返す。

## ジョブ・設定

| Method | Path | 内容 |
|---|---|---|
| GET | /api/jobs?state=... | ジョブ一覧 |
| POST | /api/jobs/{id}/cancel | キャンセル |
| GET / PATCH | /api/settings | 設定の取得・変更 (→ docs/02 settings キー一覧) |
| WS | /api/ws | ジョブ進捗・新規アセット通知 (JSON メッセージ) |

WS メッセージ例: `{"type":"job", "id":..., "state":"running", "progress":0.42}`,
`{"type":"assets_added", "bucket_keys":["2026-07-30"]}` (クライアントはバケット単位で再取得)。

ブラウザの WS は認証 Cookie、非ブラウザクライアントは WebSocket handshake の
`Authorization` header で認証する。**device token を URL query に含めてはならない**。
WS の `Origin`、接続数、frame/message size を検査・制限する。

## 共通の入力・資源制限

- JSON body: 256 KiB。配列入力 (`hashes`, `asset_ids`, `face_ids`, stack pages 等) は
  endpoint ごとに固定上限を設ける。
- 文字列入力: endpoint ごとに文字数と byte 数を制限し、NUL/control character を拒否する。
- 設定値: concurrency は 1〜64 の範囲など、実行時の資源量へ直結する値を server/core の
  両境界で検証する。
- CPU・メモリ・DB を長時間占有する処理は同時実行数を制限し、重い処理はジョブキューへ送る。
- API の 404/405/413/429/5xx も JSON error envelope で返す。

## Vault (→ docs/06)

パスは `/api/vault/...`。アンロック中のみ有効。**ロック中は全て 404**。

| Method | Path | 内容 |
|---|---|---|
| POST | /api/vault/unlock | `{password}` → `{vault_session, expires_at}` |
| POST | /api/vault/lock | セッション破棄 |
| GET | /api/vault/status | ロック状態 (これは認証済なら 200 で返す) |
| POST | /api/vault/import | `{asset_ids[]}` メイン → vault 移動 |
| POST | /api/vault/export | `{asset_ids[]}` vault → メイン移動 |

さらに、メイン側と同型の閲覧系 API を vault プレフィクスで提供する:
`/api/vault/timeline/...`, `/api/vault/assets/{id}/...` (**original のダウンロード可**),
`/api/vault/search`, `/api/vault/stacks/...`, `/api/vault/clusters/...`。
実装はサービス層を「メイン DB / vault DB」のどちらに束ねるかだけの差にする。

## 互換性ポリシー

- v1 の間は破壊的変更可。ただし docs/03 (本書) を先に更新してから実装を変えること。
- ML 系サイドカー内部 API は `/ml/v1/...` (→ docs/07) で本書のスコープ外。
