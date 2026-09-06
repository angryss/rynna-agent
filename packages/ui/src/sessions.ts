import type { Message } from './contracts';

export interface Session {
  id: string;
  name: string;
  profile: string;
  project: string | null;
  messages: Message[];
  created_at: string;
  updated_at: string;
}

const STORAGE_KEY = 'rynna-sessions-v1';

export function sessionName(prompt: string): string {
  const normalized = prompt.replace(/\s+/g, ' ').trim();
  return normalized.length > 52 ? `${normalized.slice(0, 51).trimEnd()}…` : normalized;
}

export function readSessions(storage?: Pick<Storage, 'getItem'>): Session[] {
  try {
    const stored = (storage ?? window.localStorage).getItem(STORAGE_KEY);
    if (!stored) return [];
    return decodeSessions(stored);
  } catch {
    return [];
  }
}

export function writeSessions(
  sessions: Session[],
  storage?: Pick<Storage, 'getItem' | 'setItem'>,
): Session[] {
  try {
    const target = storage ?? window.localStorage;
    const stored = target.getItem(STORAGE_KEY);
    const merged = mergeSessions(sessions, stored ? decodeSessions(stored) : []);
    target.setItem(STORAGE_KEY, JSON.stringify(merged));
    return merged;
  } catch {
    // A full or unavailable local store must not prevent the conversation itself.
    return sessions;
  }
}

export function mergeSessions(preferred: Session[], additional: Session[]): Session[] {
  const byId = new Map(additional.map(session => [session.id, session]));
  for (const session of preferred) byId.set(session.id, session);
  return [...byId.values()].sort((left, right) =>
    right.updated_at.localeCompare(left.updated_at) || left.id.localeCompare(right.id));
}

export function sessionsFromStorageEvent(event: StorageEvent): Session[] | null {
  if (event.key !== STORAGE_KEY || event.newValue === null) return null;
  try {
    return decodeSessions(event.newValue);
  } catch {
    return null;
  }
}

export function reconcileProjectSessions(
  sessions: Session[],
  profile: string,
  previousProjectNames: string[],
  nextProjectNames: string[],
): Session[] {
  const nextNames = new Set(nextProjectNames);
  const removed = previousProjectNames.filter(name => !nextNames.has(name));
  const previousNames = new Set(previousProjectNames);
  const added = nextProjectNames.filter(name => !previousNames.has(name));
  if (removed.length === 0) return sessions;

  const renamedProject = removed.length === 1 && added.length === 1 ? added[0]! : null;
  const removedNames = new Set(removed);
  return sessions.map(session =>
    session.profile === profile && session.project && removedNames.has(session.project)
      ? { ...session, project: renamedProject }
      : session,
  );
}

function decodeSessions(stored: string): Session[] {
  const decoded: unknown = JSON.parse(stored);
  return Array.isArray(decoded) ? decoded.filter(isSession) : [];
}

function isSession(value: unknown): value is Session {
  if (!value || typeof value !== 'object') return false;
  const session = value as Partial<Session>;
  return typeof session.id === 'string' &&
    typeof session.name === 'string' && session.name.length > 0 &&
    typeof session.profile === 'string' &&
    (session.project === null || typeof session.project === 'string') &&
    typeof session.created_at === 'string' &&
    typeof session.updated_at === 'string' &&
    Array.isArray(session.messages) && session.messages.every(message =>
      message && typeof message === 'object' &&
      (message.role === 'user' || message.role === 'assistant') &&
      typeof message.content === 'string',
    );
}
