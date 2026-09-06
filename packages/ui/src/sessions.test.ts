import { describe, expect, it, vi } from 'vitest';

import {
  readSessions,
  reconcileProjectSessions,
  sessionName,
  writeSessions,
  type Session,
} from './sessions';

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
    const session: Session = {
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
    expect(() => writeSessions([], {
      getItem: () => null,
      setItem: () => { throw new Error('full'); },
    })).not.toThrow();

    const availableStorage = window.localStorage;
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      get: () => { throw new DOMException('blocked', 'SecurityError'); },
    });
    expect(readSessions()).toEqual([]);
    expect(() => writeSessions([])).not.toThrow();
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: availableStorage,
    });
  });

  it('merges sessions already written by another window', () => {
    const stored: Session = {
      id: 'stored',
      name: 'Stored elsewhere',
      profile: 'work',
      project: null,
      messages: [],
      created_at: '2026-09-05T12:00:00.000Z',
      updated_at: '2026-09-05T12:00:00.000Z',
    };
    const local = { ...stored, id: 'local', name: 'Local session' };
    let value = JSON.stringify([stored]);
    const storage = {
      getItem: () => value,
      setItem: (_key: string, next: string) => { value = next; },
    };

    expect(writeSessions([local], storage)).toEqual([local, stored]);
    expect(readSessions(storage)).toEqual([local, stored]);
  });

  it('moves sessions with renamed or deleted projects without losing history', () => {
    const session: Session = {
      id: 'session-1',
      name: 'Keep this session',
      profile: 'work',
      project: 'old-name',
      messages: [{ role: 'user', content: 'Keep this' }],
      created_at: '2026-09-05T12:00:00.000Z',
      updated_at: '2026-09-05T12:00:00.000Z',
    };

    const renamed = reconcileProjectSessions([session], 'work', ['old-name'], ['new-name']);
    expect(renamed[0]?.project).toBe('new-name');
    expect(renamed[0]?.messages).toEqual(session.messages);
    expect(reconcileProjectSessions([session], 'work', ['old-name'], [])[0]?.project).toBeNull();
  });
});
