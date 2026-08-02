// 本番ビルドを実 server から配信し、主要 route が実際に描画されることを検証する。
// CSP 違反 (SEC-001 の再発) が 1 件でもあればテストを失敗させる。→ docs/12_security.md
import { expect, test } from '@playwright/test';

/** SPA fallback で配信される主要 route。 */
const ROUTES = [
  '/',
  '/stacks',
  '/people',
  '/search',
  '/duplicates',
  '/trash',
  '/settings',
  '/vault'
];

/** CSP の script-src に inline を許可する指定が紛れ込んでいないこと。 */
test('server の CSP は inline script を許可しない', async ({ request }) => {
  const response = await request.get('/');
  expect(response.status()).toBe(200);
  const csp = response.headers()['content-security-policy'];
  expect(csp, 'CSP ヘッダが無い').toBeTruthy();
  const scriptSrc = csp.split(';').find((part) => part.trim().startsWith('script-src'));
  expect(scriptSrc, 'script-src ディレクティブが無い').toBeTruthy();
  // `'wasm-unsafe-eval'` は WebAssembly のコンパイルのみを許す別トークンなので許容する。
  // 部分文字列比較だと誤検知するため、ソース式単位で判定する。
  const sources = scriptSrc!.trim().split(/\s+/).slice(1);
  expect(sources).not.toContain("'unsafe-inline'");
  expect(sources).not.toContain("'unsafe-eval'");
  expect(sources).toContain("'self'");
});

/** 配信物に inline script が残っていないこと (externalize-inline-scripts の回帰検出)。 */
test('配信される HTML に inline script が無い', async ({ request }) => {
  const html = await (await request.get('/')).text();
  const inline = [...html.matchAll(/<script(?![^>]*\bsrc\b)[^>]*>([\s\S]*?)<\/script>/gi)].filter(
    (match) => match[1].trim() !== ''
  );
  expect(inline, 'inline script が残っている').toHaveLength(0);
});

for (const route of ROUTES) {
  test(`${route} が CSP 違反なく描画される`, async ({ page }) => {
    const violations: string[] = [];
    const consoleErrors: string[] = [];

    // securitypolicyviolation はブロックされた実リソースを確実に捕捉する。
    await page.addInitScript(() => {
      (window as unknown as { __cspViolations: string[] }).__cspViolations = [];
      document.addEventListener('securitypolicyviolation', (event) => {
        (window as unknown as { __cspViolations: string[] }).__cspViolations.push(
          `${event.violatedDirective} <- ${event.blockedURI}`
        );
      });
    });
    page.on('console', (message) => {
      const text = message.text();
      if (/content security policy|refused to (execute|load|apply)/i.test(text)) {
        violations.push(text);
      } else if (message.type() === 'error') {
        consoleErrors.push(text);
      }
    });
    page.on('pageerror', (error) => consoleErrors.push(String(error)));

    const response = await page.goto(route, { waitUntil: 'networkidle' });
    expect(response?.status(), `${route} が 200 を返さない`).toBe(200);

    // SPA が実際に hydrate し、レイアウトを描画したことを DOM で確認する。
    await expect(page.locator('nav')).toHaveCount(1);
    await expect(page.locator('main')).toHaveCount(1);
    expect((await page.locator('body').innerText()).trim().length).toBeGreaterThan(0);

    const inPage = await page.evaluate(
      () => (window as unknown as { __cspViolations: string[] }).__cspViolations
    );
    expect([...violations, ...inPage], `${route} で CSP 違反`).toEqual([]);
    expect(consoleErrors, `${route} で console error`).toEqual([]);
  });
}
