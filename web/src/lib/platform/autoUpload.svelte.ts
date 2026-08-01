// 自動アップロード (フォアグラウンド同期)。docs/08 §自動アップロード。
// v1 は「アプリ起動中の手動 / 起動時トリガ」に限定する。WorkManager による
// バックグラウンド常駐は将来タスク (→ docs/10)。ネイティブ (Tauri) 専用。
//
// 縮退方針:
//  - 送信済み台帳はローカル SQLite が理想だが v1 は localStorage (path→hash)。
//  - Android の MediaStore / SAF(content URI) 走査はプラグイン次第。plugin-fs の
//    readDir/readFile で扱える範囲のみ対応し、失敗時はエラー表示にフォールバック。

import { blake3 } from 'hash-wasm';
import { getApi } from '$lib/api';
import { isTauri } from './tauri';

const FOLDERS_KEY = 'illumia.upload_folders';
const LEDGER_KEY = 'illumia.upload_ledger';

const IMAGE_EXT = new Set(['jpg', 'jpeg', 'png', 'webp', 'gif', 'avif', 'bmp', 'tiff', 'tif']);

function loadFolders(): string[] {
  if (typeof localStorage === 'undefined') return [];
  try {
    return JSON.parse(localStorage.getItem(FOLDERS_KEY) ?? '[]') as string[];
  } catch {
    return [];
  }
}
function saveFolders(folders: string[]): void {
  if (typeof localStorage !== 'undefined')
    localStorage.setItem(FOLDERS_KEY, JSON.stringify(folders));
}
function loadLedger(): Record<string, string> {
  if (typeof localStorage === 'undefined') return {};
  try {
    return JSON.parse(localStorage.getItem(LEDGER_KEY) ?? '{}') as Record<string, string>;
  } catch {
    return {};
  }
}
function saveLedger(ledger: Record<string, string>): void {
  if (typeof localStorage !== 'undefined') localStorage.setItem(LEDGER_KEY, JSON.stringify(ledger));
}

function extOf(name: string): string {
  const i = name.lastIndexOf('.');
  return i >= 0 ? name.slice(i + 1).toLowerCase() : '';
}

class AutoUpload {
  folders = $state<string[]>(loadFolders());
  syncing = $state(false);
  summary = $state<string | null>(null);
  error = $state<string | null>(null);

  async addFolder(): Promise<void> {
    this.error = null;
    if (!isTauri()) {
      this.error = 'フォルダ選択はアプリ版でのみ利用できます。';
      return;
    }
    try {
      const dialog = await import('@tauri-apps/plugin-dialog');
      const picked = await dialog.open({ directory: true, multiple: false });
      if (typeof picked === 'string' && !this.folders.includes(picked)) {
        this.folders = [...this.folders, picked];
        saveFolders(this.folders);
      }
    } catch (e) {
      this.error = e instanceof Error ? e.message : 'フォルダ選択に失敗しました';
    }
  }

  removeFolder(folder: string): void {
    this.folders = this.folders.filter((f) => f !== folder);
    saveFolders(this.folders);
  }

  /** 起動時 / 手動トリガのフォアグラウンド同期。 */
  async syncNow(): Promise<void> {
    this.error = null;
    if (!isTauri()) {
      this.error = '自動アップロードはアプリ版でのみ利用できます。';
      return;
    }
    if (this.folders.length === 0) {
      this.error = '対象フォルダを追加してください。';
      return;
    }
    this.syncing = true;
    let created = 0;
    let skipped = 0;
    let failed = 0;
    const api = getApi();
    const ledger = loadLedger();
    try {
      const fs = await import('@tauri-apps/plugin-fs');
      for (const folder of this.folders) {
        let entries: { name: string; isFile?: boolean; isDirectory?: boolean }[] = [];
        try {
          entries = (await fs.readDir(folder)) as typeof entries;
        } catch {
          failed++;
          continue;
        }
        for (const entry of entries) {
          if (entry.isDirectory) continue;
          if (!IMAGE_EXT.has(extOf(entry.name))) continue;
          const path = `${folder}/${entry.name}`;
          try {
            const bytes = await fs.readFile(path);
            const hash = await blake3(bytes);
            // 台帳 or サーバーに既にあればスキップ (自動アップロードは純粋にスキップ)。
            if (ledger[path] === hash) {
              skipped++;
              continue;
            }
            const exists = await api.assetsExist([hash]);
            if (exists[hash]) {
              ledger[path] = hash;
              skipped++;
              continue;
            }
            const file = new File([bytes], entry.name);
            await api.uploadAsset(file);
            ledger[path] = hash;
            created++;
          } catch {
            failed++;
          }
        }
      }
      saveLedger(ledger);
      this.summary = `同期完了: 新規 ${created} / スキップ ${skipped}${failed ? ` / 失敗 ${failed}` : ''}`;
    } catch (e) {
      this.error = e instanceof Error ? e.message : '同期に失敗しました';
    } finally {
      this.syncing = false;
    }
  }
}

export const autoUpload = new AutoUpload();
