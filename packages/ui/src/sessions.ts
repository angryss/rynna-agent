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

export function readSessions(storage: Pick<Storage, 'getItem'> = window.localStorage): Session[] {
  try {
    const stored = storage.getItem(STORAGE_KEY);
    if (!stored) return [];
    const decoded: unknown = JSON.parse(stored);
    return Array.isArray(decoded) ? decoded.filter(isSession) : [];
  } catch {
    return [];
  }
}

export function writeSessions(
  sessions: Session[],
  storage: Pick<Storage, 'setItem'> = window.localStorage,
): void {
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(sessions));
  } catch {
    // A full or unavailable local store must not prevent the conversation itself.
  }
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
