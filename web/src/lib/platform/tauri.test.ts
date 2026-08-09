import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

import { bindNativeServer, nativeFetch } from './tauri';

beforeEach(async () => {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  await bindNativeServer('https://illumia.example.com', 'a'.repeat(32));
  invoke.mockClear();
});

describe('native request IPC bound', () => {
  it('rejects an oversized body before invoking the Rust request bridge', async () => {
    const oversized = new Uint8Array(17 * 1024 * 1024 + 1);

    await expect(
      nativeFetch('https://illumia.example.com/api/assets', {
        method: 'POST',
        body: oversized
      })
    ).rejects.toThrow('native request body exceeds the 17 MiB limit');
    expect(invoke).not.toHaveBeenCalled();
  });
});
