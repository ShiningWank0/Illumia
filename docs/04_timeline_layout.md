# 04. タイムライン表示・justified タイルレイアウト

NAS (i3-14100, 4C8T) 上のサーバーに描画コストを載せないことが最重要制約。
方式は Immich の time-bucket を踏襲する。

## 責務分担 (NAS 負荷対策の核心)

| 処理 | 実行場所 | タイミング |
|---|---|---|
| width/height/aspect_ratio の確定 | サーバー | 取り込み時に 1 回 |
| thumbhash 生成 | サーバー | 取り込み時に 1 回 |
| サムネイル 2 種 (240px / 1440px WebP) | サーバー | 取り込み時 (ジョブキュー, 並列度設定可) |
| bucket 件数集計 | サーバー | インデックススキャンのみ (軽量) |
| **justified レイアウト計算** | **クライアント** | 表示時。O(n) で数千枚でも 1ms 台 |
| 仮想スクロール | クライアント | — |

サーバーの「事前計算」は取り込み時に確定する不変メタデータのみ。
リクエスト毎のレイアウト計算・画像処理は行わない。

## ズーム 3 段階

ピンチ (タッチ) / Ctrl+スクロール (デスクトップ) / UI ボタンで切替。

| 粒度 | 区切り | タイル | 目標行高 (基準値) |
|---|---|---|---|
| day | 日ごとの見出し | justified (縦長・横長を行内で最適配置) | 大 (~240px) |
| month | 月ごとの見出し | justified | 中 (~140px) |
| year | 年ごとの見出し | **全て正方形 (center-crop)** の均一グリッド | 小 (~90px) |

- justified が有効なのは day / month のみ。year は正方形固定 (要件)。
- 目標行高は画面幅に応じてスケール。設定で係数変更可 (将来)。

## justified アルゴリズム (day / month)

Flickr 方式の行詰めを `web/src/lib/layout/justified.ts` に自前実装する
(外部依存を増やさない・将来 egui/Rust へ移植するため)。

```
入力: items[{id, ratio}], containerWidth, targetRowHeight, gap
出力: rows[{height, tiles[{id, x, width}]}]

1. 行バッファに item を追加していき、
   sum(ratio) * targetRowHeight + gap*(n-1) >= containerWidth となったら行確定
2. 行高 h = (containerWidth - gap*(n-1)) / sum(ratio) で等比スケール
   (h は targetRowHeight * [0.6, 1.6] にクランプ。極端な行を防ぐ)
3. 最終行は詰めずに targetRowHeight のまま左寄せ
4. 幅は物理ピクセルに丸め、行内の丸め誤差は最後のタイルで吸収 (誤差 ≤ 1px)
```

必須 property test (→ docs/09 テスト方針):
順序保存 / 行幅誤差 ≤1px / ratio 0.2〜5.0 の極端値 / 空入力・1 枚入力。

## 仮想スクロール

- バケット単位で管理する。初期表示時に `GET /api/timeline/buckets` で
  全バケットの `{key, count}` を取得 (数百エントリ程度の軽量 JSON)。
- 各バケットの高さは `count` と平均 ratio 仮定値から**推定**し、全体スクロール高を構成。
  実データ取得後に実測高へ差し替え、スクロール位置を補正する (Immich と同じ方式)。
- 可視域 ± 2 バケットのみ `GET /api/timeline/buckets/{key}` で実データを取得し
  レイアウト・描画。画面外に出たバケットは DOM から外す (データは LRU キャッシュ)。
- 画像は `loading="lazy"` + thumbhash プレースホルダ → 240px サムネの順で表示。
  year 粒度でも 240px サムネを使う (正方形は CSS の object-fit: cover で切る)。

## 差分更新

- 新規アップロード時、サーバーは WS で `{"type":"assets_added","bucket_keys":[...]}` を配信。
- クライアントは該当バケットのキャッシュだけ無効化して再取得。全体再計算はしない。
- バケット外への影響は件数と推定高のみなので、スクロール補正だけで済む。

## サムネイル生成 (サーバー側)

- デコード: zune-jpeg / image。リサイズ: fast_image_resize (SIMD)。エンコード: WebP (品質 80)。
- 240px: 長辺 240。1440px: 長辺 1440 (原寸がそれ以下なら原寸)。
- ジョブキューの `thumbnail` ジョブとして実行。並列度は `jobs.thumbnail_concurrency`
  (既定: 物理コア数 - 1)。ML ジョブより高優先度。
- 一括アップロード中もタイムライン API の応答を維持すること (性能要件:
  4C 制限の Docker 環境で数千枚取り込み中に bucket API p95 < 100ms)。

## パフォーマンス検証 (M1 完了条件)

- 合成データ (縦長/横長/正方形を混ぜた数千枚) を生成して一括アップロードし、
  `docker compose` の `cpus: 4` 制限下で以下を実測:
  - 3 段ズームの切替とスクロールが 60fps 近辺を維持 (クライアント側計測)
  - 取り込み中の API p95 / CPU / メモリ (`docker stats`)
- レイアウトの見た目検証: 縦長連続・横長連続・混在の 3 パターンのスクリーンショット比較。
