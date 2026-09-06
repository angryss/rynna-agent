import { describe, expect, it, vi } from 'vitest';

import { readSessions, sessionName, writeSessions } from './sessions';

describe('sessions', () => {
  it('names a session from a normalized, bounded first prompt', () => {
    expect(sessionName('  Review\n\nthis change  ')).toBe('Review this change');
    expect(sessionName('a'.repeat(60))).toBe(`${'a'.repeat(51)}…`);
  });

  it('round trips valid sessions and ignores malformed stored entries', () => {
    let value = '';
    const storage = {
      getItem: vi.fn(() => value),
      setItem: vi.fn((_key: string, next: string) => { value = next; }),
    };
    const session = {
      id: 'session-1',
      name: 'Review this change',
      profile: 'work',
      project: 'rynna',
      messages: [{ role: 'user' as const, content: 'Review this change' }],
      created_at: '2026-09-05T12:00:00.000Z',
      updated_at: '2026-09-05T12:00:00.000Z',
    };

    writeSessions([session], storage);
    expect(readSessions(storage)).toEqual([session]);
    value = JSON.stringify([session, { id: 42 }]);
    expect(readSessions(storage)).toEqual([session]);
  });

  it('treats unavailable or invalid storage as empty without throwing', () => {
    expect(readSessions({ getItem: () => { throw new Error('unavailable'); } })).toEqual([]);
    expect(readSessions({ getItem: () => '{broken' })).toEqual([]);
    expect(() => writeSessions([], { setItem: () => { throw new Error('full'); } })).not.toThrow();
  });
});
