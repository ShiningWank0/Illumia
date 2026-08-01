# 07. ML 統合 (Python サイドカー)

キャラ認識モデルは別リポジトリ
[ShiningWank0/anime_character_recognize](https://github.com/ShiningWank0/anime_character_recognize)
(以下 ACR) で開発中。v1 の Illumia は ACR を **Python サイドカー**として統合し、
将来 Rust (ort) へ移行する (→ docs/10)。この RPC 境界がそのまま移行面になる。

## ACR 側の前提 (ACR docs/04_inference_requirements.md より)

- 全モデル ONNX / onnxruntime (CPU 必須、OpenVINO / CoreML 任意)。CUDA 非依存
- パイプライン: 検出 (person/head/face) → 品質ゲート → 同一性埋め込み
  → 2 閾値オープンセットクラスタリング (τ_high / τ_low + 拒否) → ユーザー命名
- モデルは versioned bundle (manifest.yaml + *.onnx + thresholds.yaml + tag_vocab.json)
- 性能目標: 4 コア CPU で 1 画像/秒以上 (バックグラウンド前提)、ピーク RSS ≤ 4GB

## 責務分担 (最重要)

- **サイドカーはステートレス**。DB を持たず、何も保存・キャッシュしない。
  入力 (画像バイト or 埋め込み行列) → 出力 (JSON/バイナリ) のみ。
- 永続化 (faces / clusters / assignments)・ジョブ管理・ユーザー操作の反映は
  すべて Rust 側 (illumia-core) が行う。→ vault 対応 (docs/06) と Rust 移行が単純になる。
- 「自動処理がユーザーの確定 (confirmed/rejected) を上書きしない」不変条件は
  Rust 側で担保する。

## プロセス管理・通信

- 通信: **unix domain socket** (Windows all-in-one では named pipe)。TCP を開かない。
  Docker では compose の共有ボリューム上の socket を用いる。
- サイドカーは FastAPI + uvicorn (uds オプション)。Rust 側が子プロセスとして起動・監視
  (Docker では別コンテナで、healthcheck による監視)。
- `ml/` は uv 管理。ACR は `pyproject.toml` の git 依存でコミット固定 (version pin)。
- モデルバンドルは初回起動時に GitHub Releases / HF からダウンロードし
  `<data_root>/models/` にキャッシュ。ハッシュ検証 (bundle の checksums.sha256)。

## RPC 契約 (`/ml/v1/...`)

### GET /ml/v1/health
`{status, model_bundle: {name, version, sha256}, providers: ["CPUExecutionProvider", ...]}`

### POST /ml/v1/analyze — 1 画像の解析
- リクエスト: `Content-Type: application/octet-stream` (画像バイトそのまま)。
  クエリ: `?tagger=false` (v1 では tagger 省略可)
- レスポンス (JSON):

```json
{
  "model_version": "acr-v1.0",
  "instances": [
    {
      "kind": "face",
      "bbox": [0.31, 0.08, 0.22, 0.25],
      "det_conf": 0.97,
      "quality": {"passed": true, "flags": []},
      "embedding": {"dtype": "f32", "dim": 768, "b64": "..."},
      "tags": [{"slot": "hair_color", "tag": "silver_hair", "conf": 0.93}]
    }
  ]
}
```

### POST /ml/v1/cluster — クラスタリング / 割り当て
- 用途 2 種を `mode` で分ける:
  - `full`: 全埋め込みからクラスタ構築 (初回・再クラスタリング)
  - `assign`: 新規埋め込みを既存クラスタ代表 (medoid) と照合して割り当て
- リクエスト (JSON。サイドカーは msgpack も受けるが、**Rust クライアントは JSON モードを正**とする
  — 依存を増やさないため。2026-08 実装で確定): `{mode, params: {tau_high?, tau_low?, min_cluster_size?},
  embeddings: <f32 LE bytes>, shape: [n, dim], ids: [...],
  medoids?: {cluster_id: <f32 LE bytes>}, rejections?: [[id, cluster_id], ...]}`
- レスポンス: `{assignments: [{id, cluster: "c3"|null, state: "auto"|"candidate"|"unassigned",
  similarity}] , new_clusters: [{tmp_id, member_ids, medoid_ids}]}`
- 閾値パラメータ未指定時は bundle の thresholds.yaml の較正値を使う。
  Rust 側は settings のオーバーライド値 (ml.tau_high_override 等) があれば渡す。

### 将来の受け皿 (v1 では未実装。namespace だけ予約)
- `POST /ml/v1/ocr` (manga-ocr) / `POST /ml/v1/text_embed` (スマートサーチのクエリ埋め込み)
  → docs/10

## Rust 側オーケストレーション

- `ml_analyze` ジョブ: 対象 asset を 1 件ずつ analyze → faces/embeddings を DB へ。
  並列度 `jobs.ml_concurrency` (既定 1。NAS の CPU を占有しない)。
  thumbnail ジョブより低優先度。
- `ml_cluster` ジョブ: 逐次追加時は `assign` モード (medoid とだけ比較で軽量)。
  設定変更・手動トリガ時に `full` で再クラスタリング。埋め込みは DB に保存済みなので
  画像の再処理は不要 (ACR の設計通り)。
- ユーザー操作の API (→ docs/03 クラスタ節) は DB のみ更新。confirmed / rejected /
  cluster_rejections は次回 cluster 呼び出しの入力として渡す。
- モデル更新時: `model_version` が異なる埋め込みは混ぜない。新旧併存させ、
  全 asset の再解析ジョブが完了してから旧バージョンの結果を削除。

## Vault の扱い (→ docs/06)

- vault の解析はアンロック中のみ。Rust 側が blob を復号してバイトを送る。
  ファイル名・ID をサイドカーに渡さない (リクエストは匿名の連番参照のみ)。
- サイドカーのアクセスログは無効化 (uvicorn access_log=False)。
  例外時のスタックトレースにも入力データを含めない。

## 設定項目 (GUI から変更可, → docs/02 settings)

| キー | 内容 | 既定 |
|---|---|---|
| ml.tau_high_override | 自動割り当て閾値の上書き | NULL (bundle 較正値) |
| ml.tau_low_override | 候補提示の下限閾値 | NULL |
| ml.min_cluster_size | クラスタとして表示する最低枚数 | 3 |
| ml.quality_gate | 'review_only' / 'strict' | review_only |
| jobs.ml_concurrency | 解析ジョブ並列度 | 1 |
| ml.enabled | ML 機能の全体 ON/OFF (all-in-one で無効化可) | true |
| ml.socket_path | サイドカーの unix socket パス。未設定なら ML 無効 | NULL |

補足 (2026-08 実装で確定): サイドカーのプロセス管理は Rust 側では行わない
(Docker では別コンテナ + healthcheck、デスクトップ all-in-one では子プロセス起動を
デスクトップ側が担う)。`ml.enabled` かつ `ml.socket_path` 設定時のみ
ML ジョブハンドラが登録される。

## テスト要件

- サイドカーをモック化した契約テスト (Rust 側): analyze / cluster の JSON スキーマ、
  タイムアウト・再起動時のジョブ再試行
- 実モデル E2E (ACR リポジトリのフィクスチャ流用): 少数画像で検出→クラスタ→命名の一連
- ユーザー確定の不可侵テスト: confirmed の face が full 再クラスタで別クラスタへ
  自動移動しないこと
