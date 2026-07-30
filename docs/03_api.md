# 03. API 仕様 (REST + WebSocket)

illumia-server が公開する HTTP API。all-in-one 版では同じサービス層を
in-process 呼び出しするため、**ハンドラにロジックを書かないこと** (→ docs/01)。

- ベースパス: `/api`
- 認証: `Authorization: Bearer <device_token>` (初回セットアップとログインを除く)
- Vault 系は追加で `X-Vault-Session: <vault_session_token>` (→ docs/06)
- エラー: `{ "error": { "code": "string", "message": "string" } }`。
  Vault ロック中の vault 系エンドポイントは一律 **404** を返す (存在秘匿)。

## 認証

| Method | Path | 内容 |
|---|---|---|
| GET  | /api/server/info | バージョン・セットアップ済みか (未認証可) |
| POST | /api/auth/setup | 初回のみ。パスワード設定 → token 発行 |
| POST | /api/auth/login | `{password, device_name}` → `{token}` |
| GET  | /api/auth/devices | 発行済みデバイストークン一覧 |
| DELETE | /api/auth/devices/{id} | トークン失効 |

シングルユーザー。パスワードは Argon2id でハッシュ化して settings に保存。
トークンは 256bit ランダム、DB には SHA-256 のみ保存。

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

## ジョブ・設定

| Method | Path | 内容 |
|---|---|---|
| GET | /api/jobs?state=... | ジョブ一覧 |
| POST | /api/jobs/{id}/cancel | キャンセル |
| GET / PATCH | /api/settings | 設定の取得・変更 (→ docs/02 settings キー一覧) |
| WS | /api/ws | ジョブ進捗・新規アセット通知 (JSON メッセージ) |

WS メッセージ例: `{"type":"job", "id":..., "state":"running", "progress":0.42}`,
`{"type":"assets_added", "bucket_keys":["2026-07-30"]}` (クライアントはバケット単位で再取得)。

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
