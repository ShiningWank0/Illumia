import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/vaultSession.svelte', () => ({ getVaultToken: () => null }));

import { authedObjectUrl, revokeAllObjectUrls, revokeVaultObjectUrls } from './image';

let nextObjectUrl = 0;
let responseSize = 0;
const NativeURL = URL;

function responseWithSize(size: number): Response {
  return {
    ok: true,
    status: 200,
    headers: new Headers(),
    blob: async () => ({ size })
  } as unknown as Response;
}

beforeEach(() => {
  nextObjectUrl = 0;
  responseSize = 0;
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => responseWithSize(responseSize))
  );
  class MockURL extends NativeURL {}
  MockURL.createObjectURL = vi.fn(() => `blob:test-${nextObjectUrl++}`);
  MockURL.revokeObjectURL = vi.fn();
  vi.stubGlobal('URL', MockURL);
});

afterEach(() => {
  revokeAllObjectUrls();
  vi.unstubAllGlobals();
});

describe('authenticated Object URL cache', () => {
  it('evicts the least recently used blobs when the byte budget is exceeded', async () => {
    responseSize = 16 * 1024 * 1024;
    const urls = await Promise.all(
      Array.from({ length: 7 }, (_, index) => authedObjectUrl(`/api/assets/asset-${index}/preview`))
    );

    expect(fetch).toHaveBeenCalledTimes(7);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(urls[0]);
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);

    await authedObjectUrl('/api/assets/asset-0/preview');
    expect(fetch).toHaveBeenCalledTimes(8);
  });

  it('subtracts Vault bytes when Vault URLs are revoked', async () => {
    responseSize = 16 * 1024 * 1024;
    const regular = await Promise.all(
      Array.from({ length: 5 }, (_, index) =>
        authedObjectUrl(`/api/assets/regular-${index}/preview`)
      )
    );
    const vault = await authedObjectUrl('/api/vault/assets/private/preview');

    revokeVaultObjectUrls();
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(vault);

    await authedObjectUrl('/api/assets/regular-5/preview');
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);
    expect(await authedObjectUrl('/api/assets/regular-0/preview')).toBe(regular[0]);
  });

  it('rejects an oversized preview before creating an Object URL', async () => {
    responseSize = 16 * 1024 * 1024 + 1;

    await expect(authedObjectUrl('/api/assets/large/preview')).rejects.toMatchObject({
      status: 413,
      code: 'image_too_large'
    });
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it('cancels a streaming image as soon as its endpoint limit is exceeded', async () => {
    const cancel = vi.fn(async () => undefined);
    const releaseLock = vi.fn();
    const chunks = [new Uint8Array(2 * 1024 * 1024), new Uint8Array(1)];
    vi.mocked(fetch).mockResolvedValue({
      ok: true,
      status: 200,
      headers: new Headers(),
      body: {
        getReader: () => ({
          read: vi.fn(async () =>
            chunks.length > 0 ? { done: false, value: chunks.shift()! } : { done: true }
          ),
          cancel,
          releaseLock
        })
      }
    } as unknown as Response);

    await expect(authedObjectUrl('/api/assets/large/thumbnail')).rejects.toMatchObject({
      status: 413,
      code: 'image_too_large'
    });
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(releaseLock).toHaveBeenCalledTimes(1);
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it('deduplicates concurrent requests for the same URL', async () => {
    let finish: ((response: Response) => void) | undefined;
    vi.mocked(fetch).mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          finish = resolve;
        })
    );

    const first = authedObjectUrl('/api/assets/same/thumbnail');
    const second = authedObjectUrl('/api/assets/same/thumbnail');
    finish?.(responseWithSize(1024));

    await expect(Promise.all([first, second])).resolves.toEqual(['blob:test-0', 'blob:test-0']);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
  });

  it('does not restore an in-flight Vault response after lock', async () => {
    let finish: ((response: Response) => void) | undefined;
    vi.mocked(fetch).mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          finish = resolve;
        })
    );

    const pending = authedObjectUrl('/api/vault/assets/private/thumbnail');
    revokeVaultObjectUrls();
    finish?.(responseWithSize(1024));

    await expect(pending).rejects.toMatchObject({ code: 'image_cache_revoked' });
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });
});
