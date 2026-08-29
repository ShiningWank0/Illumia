// SEC-002 の回帰テスト (docs/12_security.md)。
//
// 中心となる主張は「信頼できないネットワーク上の偽 local サーバーへ
// credential を送らない」こと。ここでは接続先選択のロジックが
//  1. external を先に試す
//  2. 平文 HTTP の local を明示確認なしに選ばない
//  3. pin 済み instance_id と一致しないサーバーを採用しない
// を満たすことを検証する。

import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  isInsecureLocal,
  loadProfile,
  probeAndSelect,
  saveProfile,
  validateProfile,
  type ConnectionProfile
} from './connection';
import { parseServerUrl, ServerUrlError } from './serverUrl';

const { probeNativeServer, bindNativeServer } = vi.hoisted(() => ({
  probeNativeServer: vi.fn(),
  bindNativeServer: vi.fn()
}));
vi.mock('./tauri', () => ({ probeNativeServer, bindNativeServer }));

const REAL_INSTANCE = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const ATTACKER_INSTANCE = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';

function infoResponse(instanceId: string) {
  return {
    setup_completed: true,
    authenticated: false,
    setup_token_required: false,
    instance_id: instanceId
  };
}

/** 到達不能 (別ネットワークにいて external へ届かない状況)。 */
function unreachable() {
  return Promise.reject(new Error('network unreachable'));
}

// connection.ts は localStorage の有無を見て動くので、jsdom を足さず最小の
// インメモリ実装を差し込む。
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  clear(): void {
    this.map.clear();
  }
}
const storage = new MemoryStorage();
vi.stubGlobal('localStorage', storage);

beforeEach(() => {
  probeNativeServer.mockReset();
  bindNativeServer.mockReset();
  bindNativeServer.mockResolvedValue(undefined);
  storage.clear();
});

describe('URL 検証', () => {
  it('external に平文 HTTP を許可しない', () => {
    expect(() => validateProfile({ external: 'http://illumia.example.com' })).toThrow(
      ServerUrlError
    );
  });

  it('URL 埋め込みの credential を拒否する', () => {
    expect(() => parseServerUrl('https://user:pass@example.com')).toThrow(ServerUrlError);
  });

  it('fragment / query / path を拒否する', () => {
    expect(() => parseServerUrl('https://example.com#x')).toThrow(ServerUrlError);
    expect(() => parseServerUrl('https://example.com?a=1')).toThrow(ServerUrlError);
    expect(() => parseServerUrl('https://example.com/sub')).toThrow(ServerUrlError);
  });

  it('制御文字を含む URL を拒否する', () => {
    expect(() => parseServerUrl('https://exa mple.com')).toThrow(ServerUrlError);
    // URL パーサは tab / 改行を URL 内のどこにあっても黙って除去するため、
    // parse 前の生文字列で弾く必要がある (弾かないと別ホストへ化ける)。
    expect(() => parseServerUrl('https://exam\nple.com')).toThrow(ServerUrlError);
    expect(() => parseServerUrl('https://exam\tple.com')).toThrow(ServerUrlError);
    expect(() => parseServerUrl('https://exam\u0000ple.com')).toThrow(ServerUrlError);
    // 前後の空白は無害なので trim して受理する。
    expect(parseServerUrl('  https://example.com  ').url).toBe('https://example.com');
  });

  it('https 以外のスキームを拒否する', () => {
    expect(() => parseServerUrl('javascript:alert(1)')).toThrow(ServerUrlError);
    expect(() => parseServerUrl('file:///etc/passwd')).toThrow(ServerUrlError);
  });

  it('local の平文 HTTP はプライベート宛先のみ受理する', () => {
    expect(
      parseServerUrl('http://192.168.1.10:2283', { allowInsecurePrivate: true }).insecure
    ).toBe(true);
    expect(() => parseServerUrl('http://example.com', { allowInsecurePrivate: true })).toThrow(
      ServerUrlError
    );
    // `.local` は probe 後に mDNS/DNS が別IPへ変わり得るため、平文credential送信に使わない。
    expect(() => parseServerUrl('http://photos.local', { allowInsecurePrivate: true })).toThrow(
      ServerUrlError
    );
    expect(parseServerUrl('https://photos.local', { allowInsecurePrivate: true }).insecure).toBe(
      false
    );
    expect(
      parseServerUrl('http://illumia.localhost:2283', { allowInsecurePrivate: true }).insecure
    ).toBe(true);
  });

  it('改ざんされた localStorage は読み出し時に拒否する', () => {
    localStorage.setItem('illumia.connection', JSON.stringify({ external: 'http://evil.test' }));
    expect(loadProfile()).toBeNull();
  });
});

describe('接続先の選択', () => {
  const profile: ConnectionProfile = {
    external: 'https://illumia.example.com',
    local: 'http://192.168.1.10:2283'
  };

  it('external を local より先に試す', async () => {
    probeNativeServer.mockResolvedValueOnce(infoResponse(REAL_INSTANCE));
    const result = await probeAndSelect(profile, { confirmInsecureLocal: async () => true });

    expect(result.baseUrl).toBe('https://illumia.example.com');
    expect(probeNativeServer).toHaveBeenCalledTimes(1);
    expect(probeNativeServer.mock.calls[0][0]).toBe('https://illumia.example.com');
    expect(bindNativeServer).toHaveBeenCalledWith('https://illumia.example.com', REAL_INSTANCE);
  });

  it('明示確認がなければ平文 HTTP の local を試さない', async () => {
    probeNativeServer.mockImplementationOnce(unreachable);
    // confirmInsecureLocal を渡さない = 確認が取れていない
    const result = await probeAndSelect(profile);

    expect(result.baseUrl).toBeNull();
    // external の 1 回だけ。local へは 1 度も接触しない。
    expect(probeNativeServer).toHaveBeenCalledTimes(1);
  });

  it('利用者が拒否した場合は平文 HTTP の local を試さない', async () => {
    probeNativeServer.mockImplementationOnce(unreachable);
    const confirm = vi.fn(async () => false);
    const result = await probeAndSelect(profile, { confirmInsecureLocal: confirm });

    expect(confirm).toHaveBeenCalled();
    expect(result.baseUrl).toBeNull();
    expect(probeNativeServer).toHaveBeenCalledTimes(1);
  });

  it('偽 local サーバー (pin 不一致) を採用しない', async () => {
    // 別 Wi-Fi にいて external へ届かず、攻撃者が同じ private IP で応答する状況。
    probeNativeServer.mockImplementationOnce(unreachable);
    probeNativeServer.mockResolvedValueOnce(infoResponse(ATTACKER_INSTANCE));

    const result = await probeAndSelect(
      { ...profile, instanceId: REAL_INSTANCE },
      { confirmInsecureLocal: async () => true }
    );

    expect(result.baseUrl).toBeNull();
    expect(result.identityMismatch).toBe(true);
  });

  it('2xx でも Illumia の schema でなければ採用しない', async () => {
    probeNativeServer.mockResolvedValueOnce({ hello: 'world' });
    const result = await probeAndSelect(
      { external: 'https://illumia.example.com' },
      { confirmInsecureLocal: async () => true }
    );

    expect(result.baseUrl).toBeNull();
  });

  it('pin 済みサーバーと一致する local は明示確認の上で採用する', async () => {
    probeNativeServer.mockImplementationOnce(unreachable);
    probeNativeServer.mockResolvedValueOnce(infoResponse(REAL_INSTANCE));

    const result = await probeAndSelect(
      { ...profile, instanceId: REAL_INSTANCE },
      { confirmInsecureLocal: async () => true }
    );

    expect(result.baseUrl).toBe('http://192.168.1.10:2283');
    expect(result.identityMismatch).toBe(false);
  });

  it('初回接続では instance_id を pin する', async () => {
    probeNativeServer.mockResolvedValueOnce(infoResponse(REAL_INSTANCE));
    const result = await probeAndSelect({ external: 'https://illumia.example.com' });

    expect(result.pinned).toBe(REAL_INSTANCE);
  });

  it('Rust bridge が再 bind を拒否した接続先は採用しない', async () => {
    probeNativeServer.mockResolvedValueOnce(infoResponse(REAL_INSTANCE));
    bindNativeServer.mockRejectedValueOnce(new Error('binding is frozen'));

    const result = await probeAndSelect({
      external: 'https://illumia.example.com',
      instanceId: REAL_INSTANCE
    });

    expect(result.baseUrl).toBeNull();
    expect(result.identityMismatch).toBe(true);
  });
});

describe('補助', () => {
  it('平文 HTTP の local を検出する', () => {
    expect(isInsecureLocal({ external: 'https://a.test', local: 'http://10.0.0.2' })).toBe(true);
    expect(isInsecureLocal({ external: 'https://a.test', local: 'https://10.0.0.2' })).toBe(false);
    expect(isInsecureLocal({ external: 'https://a.test' })).toBe(false);
  });

  it('保存時に URL を正規化する', () => {
    const saved = saveProfile({ external: 'https://illumia.example.com/', ssid: '  home  ' });
    expect(saved.external).toBe('https://illumia.example.com');
    expect(saved.ssid).toBe('home');
  });
});
