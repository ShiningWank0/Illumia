# 13. モデル要件 (anime_character_recognize バンドル契約)

**目的**: ACR 側でモデルが完成したとき、**バンドルを配置して較正値を確認するだけ**で
Illumia の ML 機能が有効になる状態を保証する。本書は Illumia が要求する側の契約であり、
ACR リポジトリの「モデルバンドル契約」(ACR docs/04) と整合していなければならない。
差異が生じた場合は両リポジトリの docs を先に揃えてから実装を変更する。

## 配置場所と有効化

```
<data_root>/models/<bundle_name>/     # 例: models/anime_recognizer_v1/
  manifest.yaml
  detector.onnx
  crop_encoder.onnx
  thresholds.yaml
  tag_vocab.json                      # 属性タガー同梱時のみ (v1 では任意)
  checksums.sha256                       # corruption 検査用
```

- サイドカー (illumia-ml) は起動時に `ILLUMIA_MODEL_DIR` (Docker 既定 `/models`) から
  バンドルを探索する。**checksums.sha256 の全ファイル検証に失敗したバンドルはロードせず**、
  mock バックエンドへフォールバックして health に `backend: "mock"` を報告する。
- `checksums.sha256` と同じ配布物を信頼してはならない。raw checksum manifest の SHA-256 を
  bundle 外の `ILLUMIA_TRUSTED_MODEL_DIGESTS` (comma-separated 64 hex allowlist) で pin する。
  allowlist 未設定・不一致は fail closed で mock にする。
- `manifest.yaml` は raw `checksums.sha256` の外部 pin と manifest entry 自体の checksum が
  両方一致した bytes だけを YAML parse する。未信頼 YAML は parser へ渡さず、depth/recursion
  error を含む parse 失敗は候補単位で fail closed にする。
- バンドルが存在しない間も全 API は mock で動作する (開発・テスト用の決定的出力)。
  `GET /ml/v1/health` の `backend` (`"onnx"` / `"mock"`) が唯一の判定点で、
  サーバーはこれを settings UI に表示する (「モデル未設定」バナー)。
- 複数バンドルがある場合は、外部 pin と manifest entry checksum の検証だけで
  `version` 順の候補を作り、高い版から 1 件ずつ完全検証する。有効な最初の 1 件だけを保持し、
  複数候補の ONNX bytes を同時にメモリ上へ残さない。

## manifest.yaml 必須スキーマ

```yaml
name: anime_recognizer_v1
version: "1.0.0"            # semver。embeddings の model_version として DB に記録される
license: "..."
models:
  detector:
    file: detector.onnx
    input:                   # 前処理は manifest が正。コード側にハードコードしない
      layout: NCHW
      dtype: float32
      size: [640, 640]       # letterbox リサイズ
      mean: [0.0, 0.0, 0.0]
      std: [1.0, 1.0, 1.0]
      color: RGB
    outputs: [boxes, scores, classes]   # 出力テンソル名
    classes: [person, head, face]       # class index 順
    nms: { iou: 0.45, conf: 0.25 }
  crop_encoder:
    file: crop_encoder.onnx
    input: { layout: NCHW, dtype: float32, size: [448, 448],
             mean: [...], std: [...], color: RGB }
    output: embedding        # 出力テンソル名
    dim: 768                 # 埋め込み次元。DB の embeddings BLOB と一致必須
    normalize: l2            # 出力を L2 正規化するか (してある場合は none)
providers: [CPUExecutionProvider]   # 必須対応 EP。CoreML/OpenVINO は任意追加
```

- **前処理パラメータ (size/mean/std/letterbox 方式) は manifest が唯一の正**。
  モデル差し替え時に前処理がズレる事故を構造的に防ぐ (ACR 契約と同思想)。
- 検出クラスは `person/head/face` の 3 種 (DB の faces.kind と対応)。不足があれば
  faces.kind の CHECK 制約と docs/02 を先に改訂すること。
- detector/encoder の input 各辺は 1〜4096、総画素は最大 16,777,216、encoder `dim` は
  1〜4096 とする。trusted bundle でも巨大 allocation を起こせないよう manifest load 時に
  hard limit を適用する。
- checksum 対象の bundle 総量は 512 MiB を hard limit とする。検証済み ONNX bytes は path を
  reopen せず ORT session constructor へ渡し、両 session の生成成功後に Python 側の保持を解放する。

## thresholds.yaml 必須スキーマ (較正済み値)

```yaml
tau_high: 0.82      # 自動割り当て閾値 (Wilson 95% 下限で precision ≥ 0.98 を満たす値)
tau_low: 0.55       # 候補提示の下限
quality_gate:
  min_face_size: 48       # px (crop 短辺)
  min_det_conf: 0.5
  min_visibility: 0.6
min_cluster_size: 3
```

- Illumia は settings のオーバーライド (`ml.tau_high_override` 等) が NULL のとき
  この較正値を使う。**モデル到着時の「微調整」= この値を実データで確認し、
  必要なら settings からオーバーライドするだけ**にする。

## ONNX モデル要件

| 項目 | 要件 |
|---|---|
| opset / 形式 | onnxruntime (CPU EP) で bytes からロード可能な単一 .onnx。v1 は TOCTOU を避けるため外部データファイル不可 |
| 実行環境 | **CPUExecutionProvider で動作することが必須要件** (CUDA 前提禁止)。CoreML/OpenVINO は任意 |
| detector | 画像 1 枚 → 可変数の {bbox, score, class}。バッチ 1 固定でよい |
| crop_encoder | crop 1 枚 → 固定次元 float32 埋め込み。dim は manifest 宣言と一致 |
| 性能予算 (4C CPU) | 検出 ≤ 500ms/画像、埋め込み ≤ 300ms/crop (v1)。超える場合は ml.concurrency=1 の直列動作でも UI を阻害しないが、スループット目標 (1 画像/秒) を下回る旨を README に明記する |
| メモリ | サイドカーのピーク RSS ≤ 4GB。モデルは遅延ロード・アイドル時アンロード可能であること |
| 決定性 | 同一入力 → 同一出力 (許容誤差 1e-4)。較正値の再現性のため |

detector 出力は confidence 上位 4096 件だけを NMS に渡し、最終 instance は最大 256 件とする。
この上限を超える model output をそのまま二乗時間 NMS や response 生成へ渡してはならない。

## クラスタリング要件 (サイドカーの `cluster` API が実装する側)

- 2 閾値オープンセット方式: 類似度 ≥ tau_high → auto、tau_low〜tau_high → candidate、
  未満 → unassigned。**判定不能を拒否できることが最重要要件** (ACR 思想)。
- ユーザーの confirmed / rejected / cluster_rejections を入力として尊重し、
  自動処理がユーザー確定を覆す出力を返してはならない (Rust 側でも二重に担保)。
- 類似度は埋め込みの cosine。medoid (クラスタ代表) 数個との最大値で判定。

## モデル到着時の手順 (チェックリスト)

1. バンドルを application data と分離した model root に配置 (Docker は model-only volume。デスクトップ all-in-one は
   設定画面の「モデルフォルダを開く」から)
2. `shasum -a 256 checksums.sha256` を trusted digest として deployment 設定へ pin する
3. サイドカー再起動 → `GET /ml/v1/health` で `backend: "onnx"`・`bundle.version` を確認
4. 設定画面で ML を有効化 → 全アセットの解析ジョブを実行
5. 手元データでクラスタ精度を確認し、必要なら `ml.tau_high_override` /
   `ml.tau_low_override` / `ml.min_cluster_size` を調整 (bundle 本体は書き換えない)
6. 誤統合が出る場合は quality_gate を `strict` に変更
7. モデル更新時: 新バンドル配置と trusted digest 更新 → `model_version` が変わるため全再解析ジョブが走る。
   旧バージョンの埋め込みと混在させない (docs/07)

## 互換性テスト (ACR 側へ依頼する成果物)

- バンドルには **パリティ用フィクスチャ** (入力画像 3〜5 枚 + 期待される検出結果・
  埋め込みの先頭 8 次元) を `fixtures/` として同梱することを推奨。
  illumia-ml の統合テストがこれを読み、実装乖離を CI で検知する。
