# 11. 重複管理・ゴミ箱・誤削除防止の不変条件

**このドキュメントは削除系コードの正の仕様である。ここに反する実装はレビューで必ず差し戻す。**
ライフサイクル列の定義は docs/02 参照。

## 重複アップロード (dedup)

1. アップロード時、BLAKE3 ハッシュで既存の「本体」
   (`lifecycle='active' AND duplicate_of IS NULL`) と照合する。
2. 一致した場合も**サイレント破棄しない**。新しい asset 行を
   `lifecycle='duplicate'`, `duplicate_of=<本体 id>`,
   `purge_after = now + dedup.retention_days (既定 30 日, 設定可)` で保存する。
   物理ファイルも自分専用に保存する (本体とファイル共有しない → docs/01)。
3. 重複はタイムライン・検索に出ないが、**「重複」ビューで本体とペアで閲覧できる**。
4. 重複ビューから**漫画スタックへ追加できる** (同じ絵のページが作品構成上 2 回必要な
   ケースのため)。スタック追加のトランザクション内で `lifecycle='active'`,
   `purge_after=NULL` へ昇格し、**自動削除対象から永久に外れる**。
   `duplicate_of` は昇格後も保持する (由来の記録 + hash 一意索引の除外条件)。
5. 自動アップロード (→ docs/08) の exists 照合で弾かれたものは重複として**保存しない**
   (クライアント側スキップ)。重複保存が起きるのは明示的なアップロード操作のみ。

## ゴミ箱 (trash)

1. ユーザーの削除操作は物理削除ではなく
   `lifecycle='trashed'`, `trashed_at=now`, `purge_after = now + trash.retention_days
   (ユーザー設定可, 既定 30 日)` への遷移である。
2. 期間内はゴミ箱ビューから復元できる。復元は `lifecycle='active'`,
   `trashed_at=NULL`, `purge_after=NULL` (重複由来なら `lifecycle='duplicate'` に戻し
   `purge_after` を**現在時刻から**再設定する)。
3. **タイマーは削除のたびにリセットされる**: 削除 → 復元 → 再削除の場合、
   `purge_after` は再削除時点から retention_days 後になる。
   「前のタイマーの続きから」となる実装は禁止。
   実装規約: `trashed_at` / `purge_after` は削除操作ハンドラで**必ず現在時刻から計算して
   上書き**し、復元時に**必ず NULL クリア**する。過去の値を参照するコードを書かない。
4. `retention_days` の設定変更は**以後の削除にのみ適用**する (既存の purge_after は
   書き換えない。ユーザーが短縮した瞬間に既存ゴミ箱が即消える事故を防ぐ)。
5. ゴミ箱ビューからの「完全に削除」(即時パージ) はユーザーの明示操作としてのみ提供する。

## パージジョブ (物理削除)

定期ジョブ (1 時間毎) が以下の**完全一致条件**で対象を選ぶ:

```sql
SELECT id FROM assets
WHERE lifecycle IN ('duplicate','trashed')
  AND purge_after IS NOT NULL
  AND purge_after < :now
  AND NOT EXISTS (SELECT 1 FROM stack_pages sp WHERE sp.asset_id = assets.id)
  AND NOT EXISTS (SELECT 1 FROM assets d WHERE d.duplicate_of = assets.id);
```

削除手順 (クラッシュ耐性):

1. `lifecycle='purging'` に更新 (tombstone 化。この時点で全 API から不可視)
2. 物理ファイル削除 (library 原本 → サムネ → プレビュー)
3. DB 行削除 (faces / FTS 等は FK CASCADE とトリガで同時に消える)

起動時に `purging` の残骸があれば手順 2 から再開する。
旧版が `duplicate_of` の参照先を `purging` にして残していた場合は、物理 file を
削除せず `trashed` / `duplicate` へ安全に戻す。物理削除の直前にも逆参照を再確認し、
参照先を消す順序へは進まない。
**手順 2 で削除するパスは必ず自 asset 行の `library_path` / 自 id 由来のサムネパスのみ**。
他の行のパスを計算・削除するコードを書いてはならない。

## 誤削除防止の不変条件 (必須テスト付き)

| # | 不変条件 | 必須テスト |
|---|---|---|
| I1 | `lifecycle='active'` の行はパージジョブの対象に**絶対に**ならない (SQL の WHERE で構造的に除外) | active な行だけの DB でパージを回し、ファイル・行が 1 つも消えないこと |
| I2 | 重複パージで消えるのは**後からアップロードされた側 (duplicate 行) のみ**。`duplicate_of` の参照先 (本体) はいかなる経路でも消えない | 本体+重複ペアを作り重複の期限を過ぎさせてパージ → 本体の行とファイルが無傷であること。逆参照 (本体側を duplicate 扱いする) バグを property test で否定 |
| I3 | スタック参照がある asset はパージされない (重複昇格漏れ・trashed でも同様) | スタックに入れた duplicate / trashed の期限を過ぎさせてパージ → 残ること |
| I4 | 削除→復元→再削除でタイマーがリセットされる | 再削除後の `purge_after` が「再削除時刻 + retention」に一致し、初回削除時刻に依存しないこと |
| I5 | パージは自分のファイルだけを消す (パス計算は自行由来のみ) | 同 hash の本体と重複が別ファイルとして存在し、重複パージ後に本体ファイルが開けること |
| I6 | 復元は完全に元の状態へ戻す (visible_in_timeline・スタック所属・FTS を含む) | trash → restore 後にタイムライン/検索/スタックの見え方が削除前と一致すること |

- これらのテストは `crates/illumia-core` の統合テストとして実装し、CI 必須にする。
- パージ対象選定 SQL とパージ手順は 1 モジュール (`purge.rs` 想定) に閉じ込め、
  他のコードから物理削除 API を呼べない可視性にする。

## Vault との関係

- vault 内にも同じ dedup / trash / purge 機構が vault.db 内で独立して存在する
  (照合は vault 内の hash とのみ行う。平文側 hash とは突合しない — 存在秘匿のため)。
- メイン ⇄ vault の移動 (→ docs/06) は本ドキュメントのライフサイクルとは別の
  「DB 間移動」であり、パージジョブの対象選定に影響しない。
- transfer reconciliation が削除できるのは journal に記録した source asset 自身の
  `library_path`、自身の id から生成した thumbnail/preview、または transfer UUID 専用 staging
  directory のみ。通常 purge の対象 SQL・I1〜I6 を再利用・迂回してはならず、reconciliation
  後にも I1〜I6 の統合テストを全て通す。
- transfer の source journal 作成と同じ transaction で `duplicate_of` の逆参照閉包を検証する。
  参照元を source 集合へ含めない部分 transfer は file を 1 byte も消す前に拒否する。
  file 削除直前にも同じ transaction で閉包を再検証して source rows を `purging` に lease する。
  journal 中・lease 中の asset は新規 dedup の参照先に選ばない。旧版 journal が不完全な閉包のまま
  source file 削除済みなら、残存 duplicate を transaction 内で新しい本体へ昇格・再親子付けして
  DB を収束させる。
