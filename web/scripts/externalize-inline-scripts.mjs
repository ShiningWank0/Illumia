#!/usr/bin/env node
// adapter-static が生成する HTML には SvelteKit の bootstrap が inline script として
// 埋め込まれる。server が返す CSP は `script-src 'self' 'wasm-unsafe-eval'` であり
// inline を許可しないため、そのままでは本番配信で SPA が起動しない (→ docs/12)。
//
// build 後にこのスクリプトで inline script を外部ファイルへ切り出し、`'self'` だけで
// 読み込める形にする。`'unsafe-inline'` や nonce/hash の runtime 配線は行わない。
//
// bootstrap は `document.currentScript.parentElement` を参照するので、
// classic script (type 指定なし) のまま同じ位置に置き換える必要がある。
// module script では `document.currentScript` が null になるため type="module" にはしない。

import { createHash } from 'node:crypto';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const webRoot = fileURLToPath(new URL('..', import.meta.url));
const buildDir = join(webRoot, 'build');
const scriptDir = join(buildDir, '_app', 'immutable', 'entry');

/** `<script>` 開始タグのうち src を持たないものを拾う。 */
const OPEN_TAG = /<script(?![^>]*\bsrc\b)([^>]*)>/gi;

async function* htmlFiles(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      yield* htmlFiles(path);
    } else if (entry.name.endsWith('.html')) {
      yield path;
    }
  }
}

/** 単一 HTML の inline script を外部化する。書き換え件数を返す。 */
async function externalize(htmlPath) {
  const original = await readFile(htmlPath, 'utf8');
  let out = '';
  let cursor = 0;
  let count = 0;

  OPEN_TAG.lastIndex = 0;
  for (let open = OPEN_TAG.exec(original); open; open = OPEN_TAG.exec(original)) {
    const attrs = open[1];
    // JSON-LD 等の非実行 script は CSP の対象外なので触らない。
    if (/\btype\s*=\s*["']?(?!module|text\/javascript|application\/javascript)/i.test(attrs)) {
      continue;
    }
    const bodyStart = open.index + open[0].length;
    const bodyEnd = original.indexOf('</script>', bodyStart);
    if (bodyEnd === -1) {
      throw new Error(`${relative(webRoot, htmlPath)}: 閉じられていない <script> がある`);
    }
    const body = original.slice(bodyStart, bodyEnd);
    if (body.trim() === '') {
      continue;
    }

    const hash = createHash('sha256').update(body).digest('hex').slice(0, 16);
    const name = `bootstrap.${hash}.js`;
    await writeFile(join(scriptDir, name), body, 'utf8');

    out += original.slice(cursor, open.index);
    out += `<script src="/_app/immutable/entry/${name}"></script>`;
    cursor = bodyEnd + '</script>'.length;
    count += 1;
    OPEN_TAG.lastIndex = cursor;
  }

  if (count === 0) {
    return 0;
  }
  out += original.slice(cursor);
  await writeFile(htmlPath, out, 'utf8');
  return count;
}

/** 外部化後に inline script が残っていないことを検証する (CSP 回帰の検出)。 */
async function assertNoInlineScripts() {
  const offenders = [];
  for await (const htmlPath of htmlFiles(buildDir)) {
    const html = await readFile(htmlPath, 'utf8');
    OPEN_TAG.lastIndex = 0;
    for (let open = OPEN_TAG.exec(html); open; open = OPEN_TAG.exec(html)) {
      const bodyStart = open.index + open[0].length;
      const bodyEnd = html.indexOf('</script>', bodyStart);
      if (bodyEnd !== -1 && html.slice(bodyStart, bodyEnd).trim() !== '') {
        offenders.push(relative(webRoot, htmlPath));
        break;
      }
    }
  }
  if (offenders.length > 0) {
    throw new Error(`inline script が残っている: ${offenders.join(', ')}`);
  }
}

let total = 0;
for await (const htmlPath of htmlFiles(buildDir)) {
  total += await externalize(htmlPath);
}
await assertNoInlineScripts();
console.log(`externalize-inline-scripts: ${total} 件の inline script を外部化した`);
