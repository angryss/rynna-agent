import { describe, expect, it, vi } from 'vitest';

import {
  deleteSession,
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

    expect(writeSessions([local], storage).sessions).toEqual([local, stored]);
    expect(readSessions(storage)).toEqual([local, stored]);
  });

  it('keeps deleted sessions out of reloads and stale window saves while preserving other sessions', () => {
    const first: Session = {
      id: 'delete-me', name: 'Delete me', profile: 'work', project: null,
      messages: [{ role: 'user', content: 'Private transcript' }],
      created_at: '2026-09-05T12:00:00.000Z', updated_at: '2026-09-05T12:00:00.000Z',
    };
    const second = { ...first, id: 'keep-me', name: 'Keep me', profile: 'personal' };
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    };
    writeSessions([first, second], storage);
    expect(deleteSession(first.id, [first], storage)).toEqual([second]);
    expect(readSessions(storage)).toEqual([second]);
    expect(JSON.parse(values.get('rynna-sessions-v1')!)).toEqual([second]);
    expect(writeSessions([first, second], storage).sessions).toEqual([second]);
    deleteSession(second.id, [first, second], storage);
    expect(writeSessions([first, second], storage).sessions).toEqual([]);
    expect(readSessions(storage)).toEqual([]);
  });

  it('reports deletion failure when its durable marker cannot be saved', () => {
    expect(() => deleteSession('session-1', [], {
      getItem: () => null,
      setItem: () => { throw new Error('Storage full'); },
    })).toThrow('Storage full');
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

  it('reports that a full store did not persist the transcript', () => {
    const session: Session = {
      id: 'unsaved', name: 'Unsaved', profile: 'work', project: null,
      messages: [{ role: 'user', content: 'Please keep this' }],
      created_at: '2026-09-05T12:00:00.000Z', updated_at: '2026-09-05T12:00:00.000Z',
    };
    const full = {
      getItem: () => null,
      setItem: () => { throw new DOMException('exceeded the quota', 'QuotaExceededError'); },
    };

    const result = writeSessions([session], full);
    // The conversation must survive in memory, but the caller has to learn it is unsaved:
    // the server keeps no history, so an unreported failure loses the transcript on reload.
    expect(result.persisted).toBe(false);
    expect(result.failure).toBe('quota');
    expect(result.sessions).toEqual([session]);
  });

  it('distinguishes a blocked store from a full one', () => {
    // Deleting conversations cannot fix a SecurityError, so it must not be advised.
    const blocked = {
      getItem: () => null,
      setItem: () => { throw new DOMException('denied', 'SecurityError'); },
    };
    const session: Session = {
      id: 'blocked', name: 'Blocked', profile: 'work', project: null, messages: [],
      created_at: '2026-09-05T12:00:00.000Z', updated_at: '2026-09-05T12:00:00.000Z',
    };

    expect(writeSessions([session], blocked).failure).toBe('unavailable');
  });

  it('reports success when the store accepts the write', () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    };
    const session: Session = {
      id: 'saved', name: 'Saved', profile: 'work', project: null, messages: [],
      created_at: '2026-09-05T12:00:00.000Z', updated_at: '2026-09-05T12:00:00.000Z',
    };

    expect(writeSessions([session], storage).persisted).toBe(true);
  });
});
