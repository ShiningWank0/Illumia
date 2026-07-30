import { describe, expect, it } from 'vitest';

import { appName, getWelcomeMessage } from './app';

describe('app metadata', () => {
  it('provides the application name and welcome message', () => {
    expect(appName).toBe('Illumia');
    expect(getWelcomeMessage(appName)).toBe('Illumia の Web クライアント');
  });
});
