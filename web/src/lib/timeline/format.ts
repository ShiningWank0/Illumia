// バケットキーを日本語ロケールの見出し文字列に変換する。
// key: day=`YYYY-MM-DD`, month=`YYYY-MM`, year=`YYYY`。

import type { Granularity } from '$lib/api/types';

const WEEKDAYS = ['日', '月', '火', '水', '木', '金', '土'];

/** バケットキー → 日本語見出し。 */
export function bucketLabel(granularity: Granularity, key: string): string {
  const parts = key.split('-');
  const year = Number(parts[0]);
  if (granularity === 'year') {
    return `${year}年`;
  }
  const month = Number(parts[1]);
  if (granularity === 'month') {
    return `${year}年${month}月`;
  }
  const day = Number(parts[2]);
  // 曜日を UTC ベースで求める (bucket key は日付のみ)。
  const wd = WEEKDAYS[new Date(Date.UTC(year, month - 1, day)).getUTCDay()];
  return `${year}年${month}月${day}日(${wd})`;
}
