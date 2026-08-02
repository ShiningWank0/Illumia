#!/usr/bin/env node
// E2E 用に実 illumia-server を起動する。ビルド済み web/build を実際に配信させ、
// 本番と同じ CSP ヘッダ経路を通す (→ web/e2e/csp.spec.ts)。
// データディレクトリは毎回使い捨てにし、既存の開発用 DB を触らない。

import { spawn } from 'node:child_process';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const webRoot = fileURLToPath(new URL('..', import.meta.url));
const repoRoot = join(webRoot, '..');

const binary =
  process.env.ILLUMIA_SERVER_BIN ?? join(repoRoot, 'target', 'release', 'illumia-server');
const dataDir = await mkdtemp(join(tmpdir(), 'illumia-e2e-'));

const server = spawn(binary, [], {
  stdio: 'inherit',
  env: {
    ...process.env,
    ILLUMIA_DATA_DIR: dataDir,
    ILLUMIA_ADDR: process.env.ILLUMIA_ADDR ?? '127.0.0.1:2283',
    ILLUMIA_WEB_DIST: join(webRoot, 'build'),
    // E2E は平文 HTTP の loopback で走るので Secure 属性を外す (本番既定は true)。
    ILLUMIA_SECURE_COOKIES: 'false'
  }
});

const forward = (signal) => () => server.kill(signal);
process.on('SIGINT', forward('SIGINT'));
process.on('SIGTERM', forward('SIGTERM'));
server.on('exit', (code) => process.exit(code ?? 0));
