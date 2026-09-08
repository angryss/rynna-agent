import type { Message } from './contracts';

export interface Session {
  id: string;
  workflow_id?: string;
  workflow_run_id?: string;
  workflow_event_ids?: string[];
  name: string;
  name_source?: 'derived' | 'llm' | 'user';
  profile: string;
  project: string | null;
  messages: Message[];
  created_at: string;
  updated_at: string;
}

const STORAGE_KEY = 'rynna-sessions-v1';
const DELETED_PREFIX = 'rynna-deleted-session-v1:';

export function sessionName(prompt: string): string {
  const normalized = prompt.replace(/\s+/g, ' ').trim();
  return normalized.length > 52 ? `${normalized.slice(0, 51).trimEnd()}…` : normalized;
}

export function readSessions(storage?: Pick<Storage, 'getItem'>): Session[] {
  try {
    const stored = (storage ?? window.localStorage).getItem(STORAGE_KEY);
    if (!stored) return [];
    return decodeSessions(stored).filter(session => !isSessionDeleted(session.id, storage));
  } catch {
    return [];
  }
}

export interface WriteResult {
  sessions: Session[];
  /** False when the store rejected the write, so this transcript is not saved. */
  persisted: boolean;
}

export function writeSessions(
  sessions: Session[],
  storage?: Pick<Storage, 'getItem' | 'setItem'>,
): WriteResult {
  let merged = sessions;
  try {
    const target = storage ?? window.localStorage;
    const stored = target.getItem(STORAGE_KEY);
    merged = mergeSessions(sessions, stored ? decodeSessions(stored) : [])
      .filter(session => !isSessionDeleted(session.id, target));
    target.setItem(STORAGE_KEY, JSON.stringify(merged));
    return { sessions: merged, persisted: true };
  } catch {
    // A full or unavailable local store must not prevent the conversation itself,
    // but the caller has to tell the user: the server keeps no history, so an
    // unreported failure here loses the transcript silently.
    return { sessions: merged, persisted: false };
  }
}

// Separate keys keep concurrent deletions from overwriting one another.
export function isSessionDeleted(id: string, storage?: Pick<Storage, 'getItem'>): boolean {
  try {
    return (storage ?? window.localStorage).getItem(`${DELETED_PREFIX}${id}`) === 'true';
  } catch {
    return false;
  }
}

export function deleteSession(id: string, sessions: Session[], storage?: Pick<Storage, 'getItem' | 'setItem'>): Session[] {
  const target = storage ?? window.localStorage;
  // Do not report success if the durable deletion marker cannot be saved.
  target.setItem(`${DELETED_PREFIX}${id}`, 'true');
  return writeSessions(sessions.filter(session => session.id !== id), target).sessions;
}

export function mergeSessions(preferred: Session[], additional: Session[]): Session[] {
  const byId = new Map(additional.map(session => [session.id, session]));
  for (const session of preferred) byId.set(session.id, session);
  return [...byId.values()].sort((left, right) =>
    right.updated_at.localeCompare(left.updated_at) || left.id.localeCompare(right.id));
}

export function sessionsFromStorageEvent(event: StorageEvent): Session[] | null {
  if (event.key?.startsWith(DELETED_PREFIX)) return readSessions();
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
  if ((session.workflow_id !== undefined && typeof session.workflow_id !== 'string') ||
      (session.workflow_run_id !== undefined && typeof session.workflow_run_id !== 'string') ||
      (session.workflow_event_ids !== undefined && (!Array.isArray(session.workflow_event_ids) || !session.workflow_event_ids.every(id => typeof id === 'string')))) return false;
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

// getRandomValues also works on self-hosted HTTP pages where randomUUID is unavailable.
export function newSessionId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
