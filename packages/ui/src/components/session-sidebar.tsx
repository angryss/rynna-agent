import { useState } from 'react';
import { Folder, MessageSquare, Plus } from 'lucide-react';

import type { Project } from '../contracts';
import type { Session } from '../sessions';
import { SessionMenu } from './session-menu';
import { Button } from './ui/button';

interface SessionSidebarProps {
  activeSessionId: string | null;
  disabled: boolean;
  onNewSession: () => void;
  onDeleteSession: (session: Session) => Promise<boolean>;
  onSelectProject: (project?: string) => void;
  onSelectSession: (session: Session) => void;
  profile: string;
  projects: Project[];
  sessions: Session[];
}

export function SessionSidebar({
  activeSessionId,
  disabled,
  onNewSession,
  onDeleteSession,
  onSelectProject,
  onSelectSession,
  profile,
  projects,
  sessions,
}: SessionSidebarProps) {
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const profileSessions = sessions
    .filter(session => session.profile === profile)
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at));
  const groups = [
    { name: 'Default project', project: null },
    ...projects.map(project => ({ name: project.name, project: project.name })),
  ];

  return (
    <aside className="session-sidebar" aria-label="Sessions">
      <div className="session-sidebar-heading">
        <div>
          <p className="eyebrow">Projects</p>
          <h2>Sessions</h2>
        </div>
        <Button
          aria-label="New session"
          disabled={disabled}
          onClick={onNewSession}
          size="icon"
          type="button"
          variant="ghost"
        >
          <Plus aria-hidden="true" size={17} />
        </Button>
      </div>
      <nav aria-label="Project sessions">
        {groups.map(group => {
          const groupSessions = profileSessions.filter(session => session.project === group.project);
          return (
            <section className="session-project" key={group.project ?? 'default'}>
              <h3>
                <button
                  disabled={disabled}
                  onClick={() => onSelectProject(group.project ?? undefined)}
                  type="button"
                >
                  <Folder aria-hidden="true" size={15} />{group.name}
                </button>
              </h3>
              {groupSessions.length > 0 ? (
                <ul>
                  {groupSessions.map(session => (
                    <li key={session.id}>
                      <div className="session-row">
                        <button
                          aria-label={session.name}
                          aria-current={session.id === activeSessionId ? 'page' : undefined}
                          disabled={disabled}
                          onClick={() => onSelectSession(session)}
                          title={session.name}
                          type="button"
                        >
                          <MessageSquare aria-hidden="true" size={13} />
                          <span>{session.name}</span>
                          <time dateTime={session.updated_at}>{relativeTime(session.updated_at)}</time>
                        </button>
                        <SessionMenu name={session.name} disabled={disabled} onDelete={() => setConfirmDelete(session.id)} />
                      </div>
                      {confirmDelete === session.id && (
                        <div className="session-delete-confirm" role="group" aria-label={`Delete ${session.name}?`}>
                          <p>Delete this session from saved history? This cannot be undone.</p>
                          <div>
                            <Button autoFocus disabled={disabled} size="sm" type="button" variant="outline" onClick={event => {
                              event.currentTarget.closest('li')?.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')?.focus();
                              setConfirmDelete(null);
                            }}>Cancel</Button>
                            <Button disabled={disabled} size="sm" type="button" variant="destructive" onClick={async () => {
                              if (await onDeleteSession(session)) setConfirmDelete(null);
                            }}>Delete</Button>
                          </div>
                        </div>
                      )}
                    </li>
                  ))}
                </ul>
              ) : <p>No sessions yet</p>}
            </section>
          );
        })}
      </nav>
    </aside>
  );
}

function relativeTime(value: string): string {
  const elapsed = Math.max(0, Date.now() - new Date(value).getTime());
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return 'now';
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  return days < 30 ? `${days}d` : `${Math.floor(days / 30)}mo`;
}
