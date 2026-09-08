import { act, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import type { AgentClient, Profile, WorkflowRun } from './contracts';
import { deleteSession, readSessions, writeSessions, type Session } from './sessions';

beforeEach(() => {
  const values = new Map<string, string>();
  Object.defineProperty(window, 'localStorage', {
    configurable: true,
    value: {
      clear: () => values.clear(),
      getItem: (key: string) => values.get(key) ?? null,
      removeItem: (key: string) => values.delete(key),
      setItem: (key: string, value: string) => values.set(key, value),
    },
  });
});

function testProfile(name: string, overrides: Partial<Profile> = {}): Profile {
  return {
    name,
    providers: [{ provider: `${name}-provider`, model: `${name}-model` }],
    active_skills: [],
    mcp_servers: [],
    capabilities: [],
    default_project_directory: '.',
    projects: [],
    subagents: [],
    ...overrides,
  };
}

function savedSession(name: string, overrides: Partial<Session> = {}): Session {
  return { id: name, name, profile: 'work', project: null, messages: [],
    created_at: '2026-09-06T12:00:00.000Z', updated_at: '2026-09-06T12:00:00.000Z', ...overrides };
}

function workflowRun(session: string, status: WorkflowRun['status']): WorkflowRun {
  return { id: `run-${session}`, status, cursor: 0, reason: null, revision: 1, uncertain: false,
    workflow: { id: 'workflow', name: 'Test workflow', description: '', revision: 1, steps: [] },
    start: { request_id: 'request', session_id: session, profile: 'work', project: null,
      selection: { provider: 'fake', model: 'fake', thinking: 'default' }, workflow_id: 'workflow',
      goal: 'Finish the task', criteria: [], limits: { steps: 50, tool_calls: 512, active_seconds: 1800 }, initial_context: '' },
    consumed: { steps: 0, tool_calls: 0, active_seconds: 0 }, events: [], verification: null };
}

function workflowClient(overrides: Partial<AgentClient> = {}): AgentClient {
  return { respond: vi.fn(), startWorkflow: vi.fn(), listWorkflowRuns: vi.fn().mockResolvedValue([]),
    listWorkflows: vi.fn().mockResolvedValue([{ id: 'workflow', name: 'Test workflow', description: '', revision: 1, read_only: true }]),
    listProfiles: vi.fn().mockResolvedValue({ default_profile: 'work', provider_ids: [], configured_profiles: [], profiles: [testProfile('work')] }), ...overrides };
}

describe('App', () => {
  it('switches between dark and light themes and remembers the selection', async () => {
    const values = new Map<string, string>();
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
      },
    });
    window.localStorage.setItem('rynna-theme', 'dark');
    const user = userEvent.setup();
    render(<App client={{ respond: vi.fn() }} />);

    expect(document.documentElement).toHaveClass('dark');
    await user.click(screen.getByRole('button', { name: 'Switch to light theme' }));

    expect(document.documentElement).not.toHaveClass('dark');
    expect(document.documentElement).toHaveClass('light');
    expect(window.localStorage.getItem('rynna-theme')).toBe('light');
    expect(screen.getByRole('button', { name: 'Switch to dark theme' })).toBeInTheDocument();
  });

  it('sends a prompt through the injected client and renders the reply', async () => {
    const client: AgentClient = {
      respond: vi.fn().mockResolvedValue({
        message: { role: 'assistant', content: 'Follow the thread.' },
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Help me plan this');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(client.respond).toHaveBeenCalledWith(
      {
        session_id: expect.any(String),
        prompt: 'Help me plan this',
        history: [],
      },
      expect.any(Function),
      expect.any(AbortSignal),
    );
    expect(await screen.findByText('Follow the thread.')).toBeInTheDocument();
    expect(within(screen.getByRole('log')).getByText('Help me plan this')).toBeInTheDocument();
    const firstRequest = vi.mocked(client.respond).mock.calls[0]![0];
    expect(firstRequest.session_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    await user.type(screen.getByLabelText('Message Rynna'), 'Continue');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(vi.mocked(client.respond).mock.calls[1]![0].session_id).toBe(firstRequest.session_id);

  });

  it('warns that conversations are no longer saved once the store is full', async () => {
    // A quota-exceeded store must not break the conversation, but the user has to
    // be told: the server keeps no history, so the transcript is lost on reload.
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: {
        clear: () => {},
        getItem: () => null,
        removeItem: () => {},
        setItem: () => { throw new DOMException('exceeded the quota', 'QuotaExceededError'); },
      },
    });
    const client: AgentClient = {
      respond: vi.fn().mockResolvedValue({
        message: { role: 'assistant', content: 'Saved nowhere.' },
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Remember this');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(await screen.findByText('Saved nowhere.')).toBeInTheDocument();
    const warning = await screen.findByRole('alert');
    expect(warning).toHaveTextContent('no longer being saved');
  });

  it('does not advise deleting conversations when the store is blocked, not full', async () => {
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: {
        clear: () => {},
        getItem: () => null,
        removeItem: () => {},
        setItem: () => { throw new DOMException('denied', 'SecurityError'); },
      },
    });
    const client: AgentClient = {
      respond: vi.fn().mockResolvedValue({
        message: { role: 'assistant', content: 'Saved nowhere.' },
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Remember this');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    const warning = await screen.findByRole('alert');
    expect(warning).toHaveTextContent('not allowing Rynna to save');
    expect(warning).not.toHaveTextContent('Delete some saved conversations');
  });

  it('follows streamed replies until the reader scrolls up, and resumes at the bottom', async () => {
    let emit: Parameters<AgentClient['respond']>[1];
    const client: AgentClient = { respond: vi.fn((_request, onDelta) => {
      emit = onDelta;
      return new Promise<never>(() => {});
    }) };
    const user = userEvent.setup();
    render(<App client={client} />);
    const conversation = screen.getByRole('region', { name: 'Conversation' });
    Object.defineProperties(conversation, {
      scrollHeight: { configurable: true, value: 1200 },
      clientHeight: { configurable: true, value: 400 },
    });
    await user.type(screen.getByLabelText('Message Rynna'), 'Investigate');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    act(() => emit?.({ kind: 'content', content: 'First part' }));
    expect(conversation.scrollTop).toBe(1200);

    conversation.scrollTop = 200;
    fireEvent.scroll(conversation);
    act(() => emit?.({ kind: 'content', content: ' Second part' }));
    expect(conversation.scrollTop).toBe(200);

    conversation.scrollTop = 800;
    fireEvent.scroll(conversation);
    Object.defineProperty(conversation, 'scrollHeight', { configurable: true, value: 1400 });
    act(() => emit?.({ kind: 'content', content: ' Third part' }));
    expect(conversation.scrollTop).toBe(1400);
  });

  it('preserves scroll opt-out when the first response is saved, but follows a selected session', async () => {
    let finish!: (value: Awaited<ReturnType<AgentClient['respond']>>) => void;
    const client: AgentClient = {
      listProfiles: vi.fn().mockResolvedValue({ default_profile: 'work', provider_ids: [],
        profiles: [testProfile('work')], configured_profiles: [] }),
      respond: vi.fn((_request, onDelta) => {
        onDelta?.({ kind: 'content', content: 'Partial answer' });
        return new Promise<Awaited<ReturnType<AgentClient['respond']>>>(resolve => { finish = resolve; });
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);
    await screen.findByRole('combobox', { name: 'Profile' });
    const conversation = screen.getByRole('region', { name: 'Conversation' });
    Object.defineProperties(conversation, {
      scrollHeight: { configurable: true, value: 1200 },
      clientHeight: { configurable: true, value: 400 },
    });
    await user.type(screen.getByLabelText('Message Rynna'), 'Investigate scroll behavior');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    await screen.findByText('Partial answer');
    conversation.scrollTop = 200;
    fireEvent.scroll(conversation);

    await act(async () => finish({ message: { role: 'assistant', content: 'Completed answer' } }));
    expect(readSessions()).toHaveLength(1);
    expect(conversation.scrollTop).toBe(200);

    await user.click(screen.getByRole('button', { name: 'New session' }));
    await user.click(screen.getByRole('button', { name: 'Investigate scroll behavior' }));
    expect(screen.getByText('Completed answer')).toBeInTheDocument();
    expect(conversation.scrollTop).toBe(1200);
  });

  it('selects a profile project and starts a new session when the project changes', async () => {
    const respond = vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Done.' } });
    const client: AgentClient = {
      respond,
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'work',
        provider_ids: ['openai'],
        profiles: [testProfile('work', {
          default_project_directory: '/projects/home',
          projects: [{
            name: 'rynna',
            directories: ['/projects/rynna', '/projects/shared'],
            default_directory: '/projects/rynna',
          }],
        })],
        configured_profiles: [],
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(await screen.findByRole('button', { name: 'rynna' }));
    expect(screen.queryByRole('combobox', { name: 'Project' })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Inspect it');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond).toHaveBeenLastCalledWith(expect.objectContaining({
      profile: 'work',
      project: 'rynna',
      session_id: expect.any(String),
    }), expect.any(Function), expect.any(AbortSignal));
    const firstSession = respond.mock.calls[0]![0].session_id;

    await user.click(screen.getByRole('button', { name: 'Default project' }));
    await user.type(screen.getByLabelText('Message Rynna'), 'Start over');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond.mock.calls[1]![0]).not.toHaveProperty('project');
    expect(respond.mock.calls[1]![0].session_id).not.toBe(firstSession);
  });

  it('persists named sessions under projects and restores them from the sidebar', async () => {
    const respond = vi.fn()
      .mockResolvedValueOnce({ message: { role: 'assistant', content: 'The project is healthy.' } })
      .mockResolvedValueOnce({ message: { role: 'assistant', content: 'A fresh answer.' } })
      .mockResolvedValueOnce({ message: { role: 'assistant', content: 'Continuing.' } });
    const client: AgentClient = {
      respond,
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'work',
        provider_ids: ['openai'],
        profiles: [testProfile('work', {
          projects: [{
            name: 'rynna',
            directories: ['/projects/rynna'],
            default_directory: '/projects/rynna',
          }],
        })],
        configured_profiles: [],
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(await screen.findByRole('button', { name: 'rynna' }));
    expect(screen.queryByRole('combobox', { name: 'Project' })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Review Rynna changes');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByRole('button', { name: 'Review Rynna changes' })).toHaveAttribute('aria-current', 'page');
    const firstSession = respond.mock.calls[0]![0].session_id;

    await user.click(screen.getByRole('button', { name: 'Default project' }));
    expect(screen.queryByText('The project is healthy.')).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Plan something else');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByRole('button', { name: 'Plan something else' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Review Rynna changes' }));
    expect(screen.getByText('The project is healthy.')).toBeInTheDocument();
    expect(screen.queryByText('A fresh answer.')).not.toBeInTheDocument();
    expect(within(screen.getByRole('complementary', { name: 'Active profile' })).getByText('rynna · /projects/rynna')).toBeInTheDocument();

    await user.type(screen.getByLabelText('Message Rynna'), 'Continue the review');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond).toHaveBeenLastCalledWith(expect.objectContaining({
      session_id: firstSession,
      project: 'rynna',
      history: [
        { role: 'user', content: 'Review Rynna changes' },
        { role: 'assistant', content: 'The project is healthy.' },
      ],
    }), expect.any(Function), expect.any(AbortSignal));
  });

  it('confirms deletion, preserves other chats, and resets the active session in its project', async () => {
    const respond = vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Done.' } });
    const client: AgentClient = {
      respond,
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'work', provider_ids: [], configured_profiles: [],
        profiles: [testProfile('work', { projects: [{ name: 'rynna', directories: ['/rynna'], default_directory: '/rynna' }] })],
      }),
    };
    const user = userEvent.setup();
    const rendered = render(<App client={client} />);
    await user.click(await screen.findByRole('button', { name: 'rynna' }));
    await user.type(screen.getByLabelText('Message Rynna'), 'First chat');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    const firstId = respond.mock.calls[0]![0].session_id;
    await user.click(screen.getByRole('button', { name: 'New session' }));
    await user.type(screen.getByLabelText('Message Rynna'), 'Second chat');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    await user.click(screen.getByRole('button', { name: 'More options for First chat' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(readSessions()).toHaveLength(2);
    await user.click(screen.getByRole('button', { name: 'More options for First chat' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(screen.queryByRole('button', { name: 'First chat' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Second chat' })).toHaveAttribute('aria-current', 'page');
    await user.click(screen.getByRole('button', { name: 'More options for Second chat' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(readSessions()).toEqual([]);
    expect(within(screen.getByRole('log')).queryByText('Done.')).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Fresh chat');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond.mock.calls[2]![0]).toMatchObject({ project: 'rynna', history: [] });
    expect(respond.mock.calls[2]![0].session_id).not.toBe(firstId);
    rendered.unmount();
    render(<App client={client} />);
    expect(await screen.findByRole('button', { name: 'Fresh chat' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Second chat' })).not.toBeInTheDocument();
  });

  it.each(['resolve', 'reject'])('releases a remotely deleted request without letting its late %s unlock a new request', async (outcome) => {
    let finish!: (value: { message: { role: 'assistant'; content: string } }) => void;
    let fail!: (error: Error) => void;
    let finishNew!: typeof finish;
    const respond = vi.fn()
      .mockResolvedValueOnce({ message: { role: 'assistant', content: 'First answer' } })
      .mockImplementationOnce(() => new Promise((resolve, reject) => { finish = resolve; fail = reject; }))
      .mockImplementationOnce(() => new Promise(resolve => { finishNew = resolve; }));
    const user = userEvent.setup();
    render(<App client={{ respond }} />);
    await user.type(screen.getByLabelText('Message Rynna'), 'Saved chat');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    await user.type(screen.getByLabelText('Message Rynna'), 'Continue');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(screen.getByRole('button', { name: 'More options for Saved chat' })).toBeDisabled();
    const id = readSessions()[0]!.id;
    act(() => {
      deleteSession(id, readSessions());
      window.dispatchEvent(new StorageEvent('storage', { key: `rynna-deleted-session-v1:${id}`, newValue: 'true' }));
    });
    expect(screen.getByRole('button', { name: 'New session' })).toBeEnabled();
    await user.type(screen.getByLabelText('Message Rynna'), 'Fresh request');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond).toHaveBeenCalledTimes(3);
    expect(respond.mock.calls[2]![0]).toMatchObject({ prompt: 'Fresh request', history: [] });
    expect(respond.mock.calls[2]![0].session_id).not.toBe(id);
    await act(async () => {
      if (outcome === 'resolve') finish({ message: { role: 'assistant', content: 'Late answer' } });
      else fail(new Error('Late failure'));
    });
    expect(screen.getByRole('button', { name: 'Stop' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'New session' })).toBeDisabled();
    expect(screen.queryByText('Late answer')).not.toBeInTheDocument();
    expect(screen.queryByText('Late failure')).not.toBeInTheDocument();
    expect(screen.queryByText('First answer')).not.toBeInTheDocument();
    expect(readSessions()).toEqual([]);
    await act(async () => { finishNew({ message: { role: 'assistant', content: 'Fresh answer' } }); });
    expect(screen.getByRole('button', { name: 'New session' })).toBeEnabled();
    expect(readSessions().map(session => session.name)).toEqual(['Fresh request']);
  });

  it('deletes ordinary history even when workflow storage is unavailable', async () => {
    writeSessions([savedSession('Ordinary chat', { profile: '' })]);
    const listWorkflowRuns = vi.fn().mockRejectedValue(new Error('Workflow storage unavailable'));
    const user = userEvent.setup();
    render(<App client={{ respond: vi.fn(), listWorkflowRuns }} />);
    await user.click(screen.getByRole('button', { name: 'More options for Ordinary chat' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(readSessions()).toEqual([]);
    expect(listWorkflowRuns).not.toHaveBeenCalled();
  });

  it('deletes unrelated ordinary and terminal sessions while the active workflow runs', async () => {
    writeSessions([savedSession('Active', { workflow_id: 'workflow' }), savedSession('Ordinary'),
      savedSession('Finished', { workflow_run_id: 'run-Finished' })]);
    const client = workflowClient({ listWorkflowRuns: vi.fn(async (_profile, id) =>
      id === 'Active' ? [workflowRun(id, 'running')] : id === 'Finished' ? [workflowRun(id, 'completed')] : []) });
    const user = userEvent.setup();
    render(<App client={client} />);
    await user.click(await screen.findByRole('button', { name: 'Active' }));
    await screen.findByRole('button', { name: 'Pause' });
    for (const name of ['Ordinary', 'Finished']) {
      await user.click(screen.getByRole('button', { name: `More options for ${name}` }));
      await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
      await user.click(screen.getByRole('button', { name: 'Delete' }));
      expect(screen.queryByRole('button', { name })).not.toBeInTheDocument();
    }
    expect(readSessions().map(session => session.id)).toEqual(['Active']);
    expect(screen.getByRole('button', { name: 'Active' })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('button', { name: 'Pause' })).toBeEnabled();
  });

  it('shows workflow setup without the chat composer and restores a chat draft when switching back', async () => {
    const user = userEvent.setup();
    render(<App client={workflowClient()} />);
    await screen.findByRole('option', { name: 'Test workflow' });
    await user.type(screen.getByLabelText('Message Rynna'), 'Keep this draft');
    await user.selectOptions(screen.getByRole('combobox', { name: 'Workflow' }), 'workflow');
    expect(screen.getByLabelText('Goal')).toBeInTheDocument();
    expect(screen.getByRole('log')).toBeEmptyDOMElement();
    expect(screen.queryByLabelText('Message Rynna')).not.toBeInTheDocument();
    expect(screen.queryByText('What should we work through?')).not.toBeInTheDocument();
    await user.selectOptions(screen.getByRole('combobox', { name: 'Workflow' }), '');
    expect(screen.getByLabelText('Message Rynna')).toHaveValue('Keep this draft');
    expect(screen.getByText('What should we work through?')).toBeInTheDocument();
  });

  it('keeps conversation history and workflow output visible without the ordinary composer', async () => {
    writeSessions([savedSession('Active', { workflow_id: 'workflow', messages: [{ role: 'user', content: 'Earlier discussion' }] })]);
    const run = { ...workflowRun('Active', 'running'), events: [{ id: 1, content: 'Implementation progress' }] };
    const user = userEvent.setup();
    render(<App client={workflowClient({ listWorkflowRuns: vi.fn().mockResolvedValue([run]) })} />);
    await user.click(await screen.findByRole('button', { name: 'Active' }));
    await screen.findByRole('button', { name: 'Pause' });
    expect(screen.getByText('Earlier discussion')).toBeInTheDocument();
    expect(screen.getByText('Implementation progress')).toBeInTheDocument();
    expect(screen.getByLabelText('Steering')).toBeInTheDocument();
    expect(screen.queryByLabelText('Message Rynna')).not.toBeInTheDocument();
  });

  it('blocks workflow starts during deletion validation', async () => {
    writeSessions([savedSession('Workflow draft', { workflow_id: 'workflow' })]);
    const client = workflowClient();
    const user = userEvent.setup();
    render(<App client={client} />);
    await user.click(await screen.findByRole('button', { name: 'Workflow draft' }));
    await user.type(await screen.findByLabelText('Goal'), 'Complete the task');
    await user.type(screen.getByLabelText('Success criteria · one per line'), 'Tests pass');
    let finish!: (runs: WorkflowRun[]) => void;
    vi.mocked(client.listWorkflowRuns!).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
    await user.click(screen.getByRole('button', { name: 'More options for Workflow draft' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    const start = screen.getByRole('button', { name: 'Start workflow' });
    expect(start).toBeDisabled();
    expect(screen.getByRole('combobox', { name: 'Workflow' })).toBeDisabled();
    fireEvent.submit(start.closest('form')!);
    expect(client.startWorkflow).not.toHaveBeenCalled();
    expect(screen.queryByLabelText('Message Rynna')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Workflow draft' })).toHaveAttribute('aria-current', 'page');
    await act(async () => { finish([]); });
    expect(readSessions()).toEqual([]);
  });

  it('requires unfinished workflows to finish or cancel before session deletion', async () => {
    const listWorkflowRuns = vi.fn().mockResolvedValue([{ status: 'paused' } as WorkflowRun]);
    writeSessions([savedSession('Workflow chat', { profile: '', workflow_id: 'workflow' })]);
    const user = userEvent.setup();
    render(<App client={{
      respond: vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Workflow history' } }),
      listWorkflowRuns,
    }} />);
    const id = readSessions()[0]!.id;
    await user.click(screen.getByRole('button', { name: 'More options for Workflow chat' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(listWorkflowRuns).toHaveBeenCalledWith('', id);
    expect(screen.getByRole('alert')).toHaveTextContent('Cancel or finish');
    expect(readSessions()).toHaveLength(1);
    listWorkflowRuns.mockRejectedValueOnce(new Error('Workflow storage unavailable'));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(screen.getByRole('alert')).toHaveTextContent('Workflow storage unavailable');
    expect(readSessions()).toHaveLength(1);
    listWorkflowRuns.mockResolvedValue([{ status: 'cancelled' } as WorkflowRun]);
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(readSessions()).toEqual([]);
  });

  it('keeps a session when persistent deletion fails', async () => {
    const user = userEvent.setup();
    render(<App client={{ respond: vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Kept answer' } }) }} />);
    await user.type(screen.getByLabelText('Message Rynna'), 'Keep chat');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    const setter = vi.spyOn(window.localStorage, 'setItem').mockImplementation(() => { throw new Error('Storage full'); });
    await user.click(screen.getByRole('button', { name: 'More options for Keep chat' }));
    await user.click(screen.getByRole('menuitem', { name: 'Delete' }));
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(screen.getByRole('alert')).toHaveTextContent('Storage full');
    expect(screen.getByRole('button', { name: 'Keep chat' })).toBeInTheDocument();
    expect(readSessions()).toHaveLength(1);
    setter.mockRestore();
  });

  it('keeps saved sessions accessible when their project is renamed', async () => {
    const profile = testProfile('work', {
      projects: [{
        name: 'old-name',
        directories: ['/projects/rynna'],
        default_directory: '/projects/rynna',
      }],
    });
    writeSessions([{
      id: 'session-1',
      name: 'Review the project',
      profile: 'work',
      project: 'old-name',
      messages: [{ role: 'user', content: 'Review the project' }],
      created_at: '2026-09-05T12:00:00.000Z',
      updated_at: '2026-09-05T12:00:00.000Z',
    }]);
    const updateProfile = vi.fn(async (_name: string, saved: Profile) => saved);
    const user = userEvent.setup();
    render(<App client={{
      respond: vi.fn(),
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'work',
        provider_ids: ['openai'],
        profiles: [profile],
        configured_profiles: [profile],
      }),
      updateProfile,
    }} />);

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Projects' }));
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Name'));
    await user.type(screen.getByLabelText('Name'), 'new-name');
    await user.click(screen.getByRole('button', { name: 'Save project' }));
    await user.click(screen.getByRole('button', { name: 'Back to chat' }));

    await user.click(screen.getByRole('button', { name: 'Review the project' }));
    expect(within(screen.getByRole('complementary', { name: 'Active profile' })).getByText('new-name · /projects/rynna')).toBeInTheDocument();
    expect(within(screen.getByRole('log')).getByText('Review the project')).toBeInTheDocument();
  });

  it('submits the prompt when Enter is pressed in the composer', async () => {
    const respond = vi.fn().mockResolvedValue({
      message: { role: 'assistant' as const, content: 'Submitted.' },
    });
    const user = userEvent.setup();
    render(<App client={{ respond }} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Send with Enter{Enter}');

    expect(respond).toHaveBeenCalledWith(
      {
        session_id: expect.any(String),
        prompt: 'Send with Enter',
        history: [],
      },
      expect.any(Function),
      expect.any(AbortSignal),
    );
    expect(await screen.findByText('Submitted.')).toBeInTheDocument();
  });

  it('collapses streamed thinking when user-facing content begins and lets the user expand it', async () => {
    const client: AgentClient = {
      respond: vi.fn(async (_request, onDelta) => {
        onDelta?.({ kind: 'thinking', content: 'Inspect the request' });
        onDelta?.({ kind: 'thinking', content: '\nCompare the fields' });
        onDelta?.({ kind: 'content', content: 'Here is the result.' });
        return { message: { role: 'assistant' as const, content: 'Here is the result.' } };
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Investigate this');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(await screen.findByText('Here is the result.')).toBeInTheDocument();
    const disclosure = screen.getByText('Thinking').closest('details');
    expect(disclosure).not.toHaveAttribute('open');

    await user.click(screen.getByText('Thinking'));

    expect(disclosure).toHaveAttribute('open');
    expect(screen.getByText(/Inspect the request/)).toHaveTextContent(
      'Inspect the request Compare the fields',
    );
  });

  it('renders the final response and collapses thinking when no content delta arrives', async () => {
    const client: AgentClient = {
      respond: vi.fn(async (_request, onDelta) => {
        onDelta?.({ kind: 'thinking', content: 'Call sw_vers' });
        onDelta?.({ kind: 'content', content: '' });
        return {
          message: {
            role: 'assistant' as const,
            content: 'The computer is running macOS 26.6.1.',
          },
        };
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Which operating system?');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(await screen.findByText('The computer is running macOS 26.6.1.')).toBeInTheDocument();
    expect(screen.getByText('Thinking').closest('details')).not.toHaveAttribute('open');
  });

  it('replaces a streamed draft with the authoritative final response', async () => {
    const client: AgentClient = {
      respond: vi.fn(async (_request, onDelta) => {
        onDelta?.({ kind: 'content', content: 'Draft answer' });
        return {
          message: { role: 'assistant' as const, content: 'Verified final answer' },
        };
      }),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Answer this');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(await screen.findByText('Verified final answer')).toBeInTheDocument();
    expect(screen.queryByText('Draft answer')).not.toBeInTheDocument();
  });

  it('shows a recoverable error when the client request fails', async () => {
    const client: AgentClient = {
      respond: vi.fn().mockRejectedValue(new Error('The local server is unavailable')),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Try this');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('The local server is unavailable');
    expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled();
  });

  it('retries a failed prompt without duplicating it in conversation history', async () => {
    const respond = vi
      .fn()
      .mockRejectedValueOnce(new Error('Try again'))
      .mockResolvedValueOnce({
        message: { role: 'assistant' as const, content: 'Recovered.' },
      });
    const user = userEvent.setup();
    render(<App client={{ respond }} />);

    await user.type(screen.getByLabelText('Message Rynna'), 'Retry this');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    await screen.findByRole('alert');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(respond).toHaveBeenNthCalledWith(
      2,
      {
        session_id: expect.any(String),
        prompt: 'Retry this',
        history: [],
      },
      expect.any(Function),
      expect.any(AbortSignal),
    );
    expect(await screen.findByText('Recovered.')).toBeInTheDocument();
    expect(within(screen.getByRole('log')).getAllByText('Retry this')).toHaveLength(1);
  });

  it('lists profiles and sends new conversations through the selected profile', async () => {
    const respond = vi.fn().mockResolvedValue({
      message: { role: 'assistant' as const, content: 'Work reply.' },
    });
    const client: AgentClient = {
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'local',
        provider_ids: ['ollama', 'openai'],
        profiles: [
          {
            name: 'local',
            providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
            active_skills: [],
            mcp_servers: [],
            capabilities: ['workspace'],
            default_project_directory: '.',
            projects: [],
            subagents: [],
          },
          {
            name: 'work',
            providers: [{ provider: 'openai', model: 'gpt-5' }],
            active_skills: ['github'],
            mcp_servers: ['github'],
            capabilities: [],
            default_project_directory: '.',
            projects: [],
            subagents: [],
          },
        ],
        configured_profiles: [
          {
            name: 'local',
            providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
            active_skills: [],
            mcp_servers: [],
            capabilities: ['workspace'],
            default_project_directory: '.',
            projects: [],
            subagents: [],
          },
          {
            name: 'work',
            providers: [{ provider: 'openai', model: 'gpt-5' }],
            active_skills: ['github'],
            mcp_servers: ['github'],
            capabilities: [],
            default_project_directory: '.',
            projects: [],
            subagents: [],
          },
        ],
      }),
      respond,
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    const profile = await screen.findByLabelText('Profile');
    expect(screen.getByText('workspace capability')).toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Use local');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    await screen.findByText('Work reply.');

    await user.click(profile);
    await user.click(screen.getByRole('option', { name: 'work' }));
    await user.type(screen.getByLabelText('Message Rynna'), 'Use work');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    expect(respond).toHaveBeenNthCalledWith(
      2,
      {
        session_id: expect.any(String),
        profile: 'work',
        prompt: 'Use work',
        history: [],
      },
      expect.any(Function),
      expect.any(AbortSignal),
    );
    expect(respond.mock.calls[1]![0].session_id).not.toBe(respond.mock.calls[0]![0].session_id);
    expect(screen.getByRole('button', { name: 'Choose model: gpt-5 · Default' })).toBeInTheDocument();
    expect(screen.getByText('github skill')).toBeInTheDocument();
    expect(screen.getByText('github MCP')).toBeInTheDocument();
  });

  it('connects a ChatGPT subscription or API key without exposing the key', async () => {
    const connectOpenAi = vi.fn().mockResolvedValue({
      connected: true,
      method: 'api_key' as const,
    });
    const client: AgentClient = {
      getOpenAiAccount: vi.fn().mockResolvedValue({ connected: false, method: null }),
      connectOpenAi,
      respond: vi.fn(),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(await screen.findByRole('button', { name: 'Connect OpenAI' }));
    expect(screen.getByRole('button', { name: 'Use ChatGPT subscription' })).toBeInTheDocument();

    await user.type(screen.getByLabelText('OpenAI API key'), 'sk-secret-value');
    expect(screen.getByLabelText('OpenAI API key')).toHaveAttribute('type', 'password');
    await user.click(screen.getByRole('button', { name: 'Save API key' }));

    expect(connectOpenAi).toHaveBeenCalledWith({ method: 'api_key', api_key: 'sk-secret-value' });
    expect(screen.queryByDisplayValue('sk-secret-value')).not.toBeInTheDocument();
    expect(await screen.findByText('Connected with API key')).toBeInTheDocument();
  });

  it('starts ChatGPT browser sign-in and reports the connected plan', async () => {
    const connectOpenAi = vi.fn().mockResolvedValue({
      connected: true,
      method: 'chatgpt' as const,
      plan: 'plus',
    });
    const client: AgentClient = {
      getOpenAiAccount: vi.fn().mockResolvedValue({ connected: false, method: null }),
      connectOpenAi,
      respond: vi.fn(),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(await screen.findByRole('button', { name: 'Connect OpenAI' }));
    await user.click(screen.getByRole('button', { name: 'Use ChatGPT subscription' }));

    expect(connectOpenAi).toHaveBeenCalledWith({ method: 'chatgpt' });
    expect(await screen.findByText('Connected with ChatGPT Plus')).toBeInTheDocument();
  });

  it('does not let stale initial account status overwrite a completed connection', async () => {
    let resolveInitialAccount!: (account: { connected: false; method: null }) => void;
    const initialAccount = new Promise<{ connected: false; method: null }>((resolve) => {
      resolveInitialAccount = resolve;
    });
    const client: AgentClient = {
      getOpenAiAccount: vi.fn().mockReturnValue(initialAccount),
      connectOpenAi: vi.fn().mockResolvedValue({
        connected: true,
        method: 'chatgpt' as const,
        plan: 'plus',
      }),
      respond: vi.fn(),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(screen.getByRole('button', { name: 'Connect OpenAI' }));
    await user.click(screen.getByRole('button', { name: 'Use ChatGPT subscription' }));
    expect(await screen.findByText('Connected with ChatGPT Plus')).toBeInTheDocument();

    resolveInitialAccount({ connected: false, method: null });

    expect(await screen.findByText('Connected with ChatGPT Plus')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Connect OpenAI' })).not.toBeInTheDocument();
  });

  it('clears a rejected API key from the UI', async () => {
    const client: AgentClient = {
      getOpenAiAccount: vi.fn().mockResolvedValue({ connected: false, method: null }),
      connectOpenAi: vi.fn().mockRejectedValue(new Error('OpenAI rejected the key')),
      respond: vi.fn(),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(await screen.findByRole('button', { name: 'Connect OpenAI' }));
    const input = screen.getByLabelText('OpenAI API key');
    await user.type(input, '«redacted:sk-…»');
    await user.click(screen.getByRole('button', { name: 'Save API key' }));

    expect(await screen.findByText('OpenAI rejected the key')).toBeInTheDocument();
    expect(input).toHaveValue('');
  });

  it('clears an API key when the account panel is dismissed', async () => {
    const client: AgentClient = {
      getOpenAiAccount: vi.fn().mockResolvedValue({ connected: false, method: null }),
      connectOpenAi: vi.fn(),
      respond: vi.fn(),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    const accountButton = await screen.findByRole('button', { name: 'Connect OpenAI' });
    await user.click(accountButton);
    await user.type(screen.getByLabelText('OpenAI API key'), '«redacted:sk-…»');
    await user.click(accountButton);
    await user.click(accountButton);

    expect(screen.getByLabelText('OpenAI API key')).toHaveValue('');
  });

  it('clears a typed API key when ChatGPT sign-in succeeds', async () => {
    const client: AgentClient = {
      getOpenAiAccount: vi.fn().mockResolvedValue({ connected: false, method: null }),
      connectOpenAi: vi.fn().mockResolvedValue({
        connected: true,
        method: 'chatgpt' as const,
        plan: 'plus',
      }),
      respond: vi.fn(),
    };
    const user = userEvent.setup();
    render(<App client={client} />);

    await user.click(await screen.findByRole('button', { name: 'Connect OpenAI' }));
    await user.type(screen.getByLabelText('OpenAI API key'), '«redacted:sk-…»');
    await user.click(screen.getByRole('button', { name: 'Use ChatGPT subscription' }));
    await user.click(await screen.findByRole('button', { name: 'Connected with ChatGPT Plus' }));

    expect(screen.getByLabelText('OpenAI API key')).toHaveValue('');
  });

  it('opens provider settings blank and adds updates and deletes Ollama', async () => {
    const listProviders = vi.fn().mockResolvedValue([]);
    const createProvider = vi.fn().mockResolvedValue({
      kind: 'ollama' as const,
      api_base: 'http://localhost:11434/v1',
    });
    const updateProvider = vi.fn().mockResolvedValue({
      kind: 'ollama' as const,
      api_base: 'http://localhost:22434/v1',
    });
    const deleteProvider = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders,
          createProvider,
          updateProvider,
          deleteProvider,
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    expect(await screen.findByRole('heading', { name: 'Provider credentials' })).toBeInTheDocument();
    expect(
      screen.getByText(/Credentials are isolated by profile/),
    ).toBeInTheDocument();
    expect(screen.getByText('No providers configured.')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Add provider' }));
    await user.clear(screen.getByLabelText('Ollama API base URL'));
    await user.type(screen.getByLabelText('Ollama API base URL'), 'http://localhost:11434/v1');
    await user.click(screen.getByRole('button', { name: 'Save provider' }));
    expect(createProvider).toHaveBeenCalledWith(
      { kind: 'ollama', api_base: 'http://localhost:11434/v1' },
      'default',
    );

    await user.click(screen.getByRole('button', { name: 'Edit Ollama' }));
    await user.clear(screen.getByLabelText('Ollama API base URL'));
    await user.type(screen.getByLabelText('Ollama API base URL'), 'http://localhost:22434/v1');
    await user.click(screen.getByRole('button', { name: 'Save provider' }));
    expect(updateProvider).toHaveBeenCalledWith(
      { kind: 'ollama', api_base: 'http://localhost:22434/v1' },
      'default',
    );

    await user.click(screen.getByRole('button', { name: 'Delete Ollama' }));
    expect(deleteProvider).toHaveBeenCalledWith('ollama', 'default');
    expect(await screen.findByText('No providers configured.')).toBeInTheDocument();
  });

  it('sorts configured providers alphabetically in settings', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([
            { kind: 'openai' as const, authentication: 'chatgpt' as const },
            { kind: 'openrouter' as const },
            { kind: 'ollama' as const, api_base: 'http://localhost:11434/v1' },
            { kind: 'anthropic' as const, authentication: 'subscription' as const },
          ]),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));

    expect((await screen.findAllByRole('heading', { level: 3 })).map((heading) => heading.textContent)).toEqual([
      'Anthropic',
      'Ollama',
      'OpenAI',
      'OpenRouter',
    ]);
  });

  it('sorts provider types alphabetically in the add-provider dropdown', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));

    const providerType = screen.getByRole('combobox', { name: 'Provider type' });
    await user.click(providerType);
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Anthropic',
      'MLX',
      'Ollama',
      'OpenAI',
      'OpenRouter',
    ]);

    await user.keyboard('{Enter}');
    expect(providerType).toHaveValue('Ollama');

    await user.clear(providerType);
    await user.type(providerType, 'Ollama');
    await user.keyboard('{Enter}');
    expect(providerType).toHaveValue('Ollama');
  });

  it('supports arrow-key selection in the provider type-ahead', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));

    const providerType = screen.getByRole('combobox', { name: 'Provider type' });
    await user.click(providerType);
    await user.keyboard('{ArrowDown}{Enter}');

    expect(providerType).toHaveValue('OpenAI');
    expect(screen.getByLabelText('OpenAI authentication')).toBeInTheDocument();
  });

  it('reopens the provider type-ahead when an arrow key follows Escape', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));

    const providerType = screen.getByRole('combobox', { name: 'Provider type' });
    await user.click(providerType);
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();

    await user.keyboard('{ArrowDown}');

    expect(screen.getByRole('listbox')).toBeInTheDocument();
    expect(providerType).toHaveAttribute('aria-activedescendant', 'provider-type-option-openai');
    expect(screen.getByRole('option', { name: 'OpenAI' })).toHaveAttribute('aria-selected', 'true');
  });

  it('filters and selects provider types by typing ahead', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));

    const providerType = screen.getByRole('combobox', { name: 'Provider type' });
    await user.type(providerType, 'open');

    expect(screen.getByRole('option', { name: 'OpenAI' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Anthropic' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Ollama' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save provider' })).toBeDisabled();

    await user.keyboard('{Enter}');

    expect(providerType).toHaveValue('OpenAI');
    expect(screen.getByLabelText('OpenAI authentication')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save provider' })).toBeEnabled();
  });

  it('adds OpenAI with either an API key or ChatGPT subscription', async () => {
    const createProvider = vi.fn().mockImplementation(async (provider) => provider);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
          createProvider,
          updateProvider: vi.fn(),
          deleteProvider: vi.fn(),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    await user.clear(screen.getByRole('combobox', { name: 'Provider type' }));
    await user.type(screen.getByRole('combobox', { name: 'Provider type' }), 'open');
    await user.keyboard('{Enter}');
    await user.selectOptions(screen.getByLabelText('OpenAI authentication'), 'api_key');
    await user.type(screen.getByLabelText('OpenAI API key'), 'sk-secret');
    await user.click(screen.getByRole('button', { name: 'Save provider' }));

    expect(createProvider).toHaveBeenCalledWith(
      { kind: 'openai', authentication: 'api_key', api_key: 'sk-secret' },
      'default',
    );
    expect(screen.queryByDisplayValue('sk-secret')).not.toBeInTheDocument();
  });

  it('adds OpenRouter without collecting or persisting its API key', async () => {
    const createProvider = vi.fn().mockImplementation(async (provider) => provider);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
          createProvider,
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    const providerType = screen.getByRole('combobox', { name: 'Provider type' });
    await user.clear(providerType);
    await user.type(providerType, 'router');
    await user.keyboard('{Enter}');

    expect(screen.getByText(/Set OPENROUTER_API_KEY/)).toBeInTheDocument();
    expect(screen.queryByLabelText(/OpenRouter API key/i)).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Save provider' }));
    expect(createProvider).toHaveBeenCalledWith({ kind: 'openrouter' }, 'default');
  });

  it('asks before reusing existing ChatGPT credentials for a provider', async () => {
    const createProvider = vi.fn().mockResolvedValue({
      kind: 'openai' as const,
      authentication: 'chatgpt' as const,
    });
    const getOpenAiAccount = vi
      .fn()
      .mockResolvedValueOnce({ connected: false, method: null })
      .mockResolvedValue({ connected: true, method: 'chatgpt' as const, plan: 'plus' });
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          getExistingOpenAiAccount: vi.fn().mockResolvedValue({
            connected: true,
            method: 'chatgpt' as const,
            plan: 'plus',
          }),
          getOpenAiAccount,
          connectOpenAi: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
          createProvider,
          updateProvider: vi.fn(),
          deleteProvider: vi.fn(),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    await user.clear(screen.getByRole('combobox', { name: 'Provider type' }));
    await user.type(screen.getByRole('combobox', { name: 'Provider type' }), 'open');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Existing ChatGPT credentials found')).toBeInTheDocument();
    expect(screen.getByText(/ChatGPT Plus is already connected/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Use existing credentials' }));
    await user.click(screen.getByRole('button', { name: 'Save provider' }));

    expect(createProvider).toHaveBeenCalledWith(
      { kind: 'openai', authentication: 'chatgpt', reuse_existing: true },
      'default',
    );
    expect(getOpenAiAccount).toHaveBeenCalledTimes(2);
    await user.click(screen.getByRole('button', { name: 'Back to chat' }));
    expect(await screen.findByText('Connected with ChatGPT Plus')).toBeInTheDocument();
  });

  it('waits for existing ChatGPT credential discovery before allowing provider creation', async () => {
    let resolveExistingAccount!: (account: { connected: false; method: null }) => void;
    const existingAccount = new Promise<{ connected: false; method: null }>((resolve) => {
      resolveExistingAccount = resolve;
    });
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          getExistingOpenAiAccount: vi.fn().mockReturnValue(existingAccount),
          listProviders: vi.fn().mockResolvedValue([]),
          createProvider: vi.fn(),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    await user.clear(screen.getByRole('combobox', { name: 'Provider type' }));
    await user.type(screen.getByRole('combobox', { name: 'Provider type' }), 'open');
    await user.keyboard('{Enter}');

    expect(screen.getByRole('button', { name: 'Save provider' })).toBeDisabled();
    await act(async () => resolveExistingAccount({ connected: false, method: null }));
    expect(screen.getByRole('button', { name: 'Save provider' })).toBeEnabled();
  });

  it('lets the user choose a new ChatGPT sign-in instead of existing credentials', async () => {
    const createProvider = vi.fn().mockResolvedValue({
      kind: 'openai' as const,
      authentication: 'chatgpt' as const,
    });
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          getExistingOpenAiAccount: vi.fn().mockResolvedValue({
            connected: true,
            method: 'chatgpt' as const,
          }),
          listProviders: vi.fn().mockResolvedValue([]),
          createProvider,
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    await user.clear(screen.getByRole('combobox', { name: 'Provider type' }));
    await user.type(screen.getByRole('combobox', { name: 'Provider type' }), 'open');
    await user.keyboard('{Enter}');
    expect(screen.getByRole('button', { name: 'Save provider' })).toBeDisabled();

    await user.click(screen.getByRole('button', { name: 'Register new credentials' }));
    expect(screen.getByText('A browser window will open so you can sign in to ChatGPT.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Save provider' }));

    expect(createProvider).toHaveBeenCalledWith(
      { kind: 'openai', authentication: 'chatgpt' },
      'default',
    );
  });

  it('adds Anthropic subscription and API-key markers without collecting credentials', async () => {
    const createProvider = vi.fn().mockImplementation(async (provider) => provider);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders: vi.fn().mockResolvedValue([]),
          createProvider,
          updateProvider: vi.fn(),
          deleteProvider: vi.fn(),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    await user.clear(screen.getByRole('combobox', { name: 'Provider type' }));
    await user.type(screen.getByRole('combobox', { name: 'Provider type' }), 'anth');
    await user.keyboard('{Enter}');
    expect(screen.getByText(/Rynna tools are disabled/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Save provider' }));

    expect(createProvider).toHaveBeenCalledWith(
      { kind: 'anthropic', authentication: 'subscription' },
      'default',
    );
    expect(screen.queryByLabelText(/Anthropic API key/i)).not.toBeInTheDocument();
  });

  it('does not let a late initial provider list overwrite a completed mutation', async () => {
    let resolveProviders: (providers: []) => void = () => {};
    const listProviders = vi.fn(
      () =>
        new Promise<[]>((resolve) => {
          resolveProviders = resolve;
        }),
    );
    const createProvider = vi.fn().mockResolvedValue({
      kind: 'ollama' as const,
      api_base: 'http://localhost:11434/v1',
    });
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProviders,
          createProvider,
          updateProvider: vi.fn(),
          deleteProvider: vi.fn(),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Add provider' }));
    await user.click(screen.getByRole('button', { name: 'Save provider' }));
    expect(await screen.findByRole('button', { name: 'Edit Ollama' })).toBeInTheDocument();

    await act(async () => resolveProviders([]));

    expect(screen.getByRole('button', { name: 'Edit Ollama' })).toBeInTheDocument();
  });

  it('keeps provider and model controls out of the profiles section', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'zeta',
            provider_ids: ['anthropic', 'beta-provider', 'ollama', 'openai'],
            profiles: [
              testProfile('zeta', {
                providers: [
                  { provider: 'openai', model: 'gpt-5' },
                  { provider: 'ollama', model: 'qwen3:8b' },
                ],
              }),
              testProfile('alpha', {
                providers: [{ provider: 'anthropic', model: 'claude-sonnet-4-6' }],
              }),
              testProfile('beta'),
            ],
            configured_profiles: [
              testProfile('zeta', {
                providers: [
                  { provider: 'openai', model: 'gpt-5' },
                  { provider: 'ollama', model: 'qwen3:8b' },
                ],
              }),
              testProfile('alpha', {
                providers: [{ provider: 'anthropic', model: 'claude-sonnet-4-6' }],
              }),
              testProfile('beta'),
            ],
          }),
          listProviders: vi.fn().mockResolvedValue([]),
          createProfile: vi.fn(),
          updateProfile: vi.fn(),
          deleteProfile: vi.fn(),
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));

    expect(screen.getByRole('navigation', { name: 'Settings' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Profiles' })).toHaveAttribute('aria-current', 'page');
    const profile = await screen.findByRole('combobox', { name: 'Profile' });
    expect(profile).toHaveValue('zeta');
    expect(screen.getByRole('heading', { name: 'Profiles' })).toBeInTheDocument();
    expect(screen.queryByLabelText('Provider')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Model')).not.toBeInTheDocument();

    await user.click(profile);
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'alpha',
      'beta',
      'zeta',
    ]);

    await user.click(screen.getByRole('option', { name: 'alpha' }));
    expect(profile).toHaveValue('alpha');
    expect(screen.getByLabelText('Name')).toHaveValue('alpha');

    await user.click(screen.getByRole('button', { name: 'Provider credentials' }));
    expect(screen.getByRole('button', { name: 'Provider credentials' })).toHaveAttribute(
      'aria-current',
      'page',
    );
    expect(screen.getByRole('heading', { name: 'Provider credentials' })).toBeInTheDocument();
    expect(screen.queryByLabelText('Name')).not.toBeInTheDocument();
  });

  it('separates profile identity, credentials, and model management into three settings sections', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['ollama', 'openai'],
            profiles: [
              testProfile('alpha', {
                providers: [
                  { provider: 'ollama', model: 'qwen3:8b' },
                  { provider: 'ollama', model: 'qwen3:14b' },
                ],
              }),
            ],
            configured_profiles: [
              testProfile('alpha', {
                providers: [
                  { provider: 'ollama', model: 'qwen3:8b' },
                  { provider: 'ollama', model: 'qwen3:14b' },
                ],
              }),
            ],
          }),
          listProviders: vi.fn().mockResolvedValue([]),
          createProfile: vi.fn(),
          updateProfile: vi.fn(),
          deleteProfile: vi.fn(),
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));

    expect(screen.getByRole('button', { name: 'Profiles' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Provider credentials' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Models' })).toBeInTheDocument();
    expect(screen.getByLabelText('Name')).toBeInTheDocument();
    expect(screen.queryByLabelText('Provider')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Model')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Models' }));

    expect(screen.getByRole('heading', { name: 'Models' })).toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: 'Profile' })).toHaveValue('alpha');
    expect(screen.getByRole('combobox', { name: 'Provider' })).toHaveValue('ollama');
    expect(screen.getByRole('checkbox', { name: 'Select qwen3:8b' })).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: 'Select qwen3:14b' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Select all' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Deselect all' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Enable selected' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Disable selected' })).toBeInTheDocument();
  });

  it('loads provider credentials for the profile selected in settings', async () => {
    const listProviders = vi.fn().mockResolvedValue([]);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['ollama'],
            profiles: [testProfile('alpha'), testProfile('beta')],
            configured_profiles: [testProfile('alpha'), testProfile('beta')],
          }),
          listProviders,
          createProfile: vi.fn(),
          updateProfile: vi.fn(),
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Provider credentials' }));
    expect(listProviders).toHaveBeenCalledWith('alpha');

    const profile = screen.getByRole('combobox', { name: 'Profile' });
    await user.click(profile);
    await user.click(screen.getByRole('option', { name: 'beta' }));

    expect(listProviders).toHaveBeenCalledWith('beta');
  });

  it.each(['ollama', 'mlx'])('adds custom %s model names and rejects duplicates', async (provider) => {
    const user = userEvent.setup();
    const updateProfile = vi.fn().mockImplementation(async (_name: string, profile: Profile) => profile);
    const profile = testProfile('alpha', {
      providers: [{ provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true }],
    });
    render(<App client={{
      respond: vi.fn(), updateProfile,
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'alpha', provider_ids: ['ollama', 'mlx'],
        profiles: [profile], configured_profiles: [profile],
      }),
    }} />);
    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Models' }));
    await user.click(screen.getByRole('combobox', { name: 'Provider' }));
    await user.click(screen.getByRole('option', { name: provider }));
    const input = screen.getByLabelText('Model name');
    const add = screen.getByRole('button', { name: 'Add model' });
    expect(add).toBeDisabled();
    await user.type(input, '   ');
    expect(add).toBeDisabled();
    await user.clear(input);
    const model = provider === 'mlx' ? 'mlx-community/Qwen3.8-27B-8bit' : 'my-local-model:latest';
    await user.type(input, ` ${model} `);
    await user.click(add);
    expect(updateProfile).toHaveBeenCalledWith('alpha', {
      ...profile, providers: [...profile.providers, { provider, model, enabled: true, default: false }],
    });
    expect(await screen.findByRole('checkbox', { name: `Select ${model}` })).toBeInTheDocument();
    expect(input).toHaveValue('');
    await user.type(input, model);
    await user.click(add);
    expect(screen.getByRole('alert')).toHaveTextContent('already configured');
    expect(updateProfile).toHaveBeenCalledTimes(1);
  });

  it('preserves the custom model draft when saving fails', async () => {
    const user = userEvent.setup();
    const profile = testProfile('alpha');
    render(<App client={{
      respond: vi.fn(), updateProfile: vi.fn().mockRejectedValue(new Error('Could not save')),
      listProfiles: vi.fn().mockResolvedValue({
        default_profile: 'alpha', provider_ids: ['ollama'],
        profiles: [profile], configured_profiles: [profile],
      }),
    }} />);
    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Models' }));
    await user.type(screen.getByLabelText('Model name'), 'my-model');
    await user.click(screen.getByRole('button', { name: 'Add model' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not save');
    expect(screen.getByLabelText('Model name')).toHaveValue('my-model');
    expect(screen.queryByRole('checkbox', { name: 'Select my-model' })).not.toBeInTheDocument();
  });

  it('creates and edits MLX with its own default endpoint', async () => {
    const user = userEvent.setup();
    const createProvider = vi.fn().mockImplementation(async (input: { kind: 'mlx'; api_base: string }) => input);
    const updateProvider = vi.fn().mockImplementation(async (input: { kind: 'mlx'; api_base: string }) => input);
    render(<App client={{ respond: vi.fn(), createProvider, updateProvider, listProviders: vi.fn().mockResolvedValue([]) }} />);
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add provider' }));
    await user.click(screen.getByRole('combobox', { name: 'Provider type' }));
    await user.click(screen.getByRole('option', { name: 'MLX' }));
    expect(screen.getByLabelText('MLX API base URL')).toHaveValue('http://127.0.0.1:8000/v1');
    await user.click(screen.getByRole('button', { name: 'Save provider' }));
    expect(createProvider).toHaveBeenCalledWith({ kind: 'mlx', api_base: 'http://127.0.0.1:8000/v1' }, 'default');
    await user.click(await screen.findByRole('button', { name: 'Edit MLX' }));
    await user.clear(screen.getByLabelText('MLX API base URL'));
    await user.type(screen.getByLabelText('MLX API base URL'), 'http://localhost:8001/v1');
    await user.click(screen.getByRole('button', { name: 'Save provider' }));
    expect(updateProvider).toHaveBeenCalledWith({ kind: 'mlx', api_base: 'http://localhost:8001/v1' }, 'default');
  });

  it('edits configured models without mutating the running profile snapshot', async () => {
    const updateProfile = vi.fn().mockImplementation(async (_name: string, profile: Profile) => profile);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['ollama'],
            profiles: [
              testProfile('alpha', {
                providers: [{ provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true }],
              }),
            ],
            configured_profiles: [
              testProfile('alpha', {
                providers: [
                  { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
                  { provider: 'ollama', model: 'qwen3:14b', enabled: true, default: false },
                ],
              }),
            ],
          }),
          createProfile: vi.fn(),
          updateProfile,
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Models' }));
    expect(screen.getByText('Saved model changes take effect after restart. Chat uses the currently running models until then.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Select all' }));
    expect(screen.getByRole('button', { name: 'Disable selected' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Deselect all' }));
    await user.click(screen.getByLabelText('Select qwen3:8b'));
    await user.click(screen.getByRole('button', { name: 'Disable selected' }));

    expect(updateProfile).toHaveBeenLastCalledWith('alpha', expect.objectContaining({
      providers: [
        { provider: 'ollama', model: 'qwen3:8b', enabled: false, default: false },
        { provider: 'ollama', model: 'qwen3:14b', enabled: true, default: true },
      ],
    }));

    await user.click(await screen.findByLabelText('Make qwen3:8b default'));
    expect(updateProfile).toHaveBeenLastCalledWith('alpha', expect.objectContaining({
      providers: [
        { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
        { provider: 'ollama', model: 'qwen3:14b', enabled: true, default: false },
      ],
    }));

    await user.click(screen.getByRole('button', { name: 'Back to chat' }));
    await user.click(screen.getByRole('button', { name: /^Choose model:/ }));
    const chatModels = screen.getByRole('dialog', { name: 'Choose provider and model' });
    expect(within(chatModels).getByRole('button', { name: 'qwen3:8b' })).toBeInTheDocument();
    expect(within(chatModels).queryByRole('button', { name: 'qwen3:14b' })).not.toBeInTheDocument();
    await user.keyboard('{Escape}');
    const runtimeSummary = screen.getByRole('complementary', { name: 'Active profile' });
    expect(within(runtimeSummary).queryByText('qwen3:8b')).not.toBeInTheDocument();
    expect(within(runtimeSummary).queryByText('qwen3:14b')).not.toBeInTheDocument();
  });

  it('keeps pending configured models out of the running chat profile until restart', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['ollama'],
            profiles: [
              testProfile('alpha', {
                providers: [
                  { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
                ],
              }),
            ],
            configured_profiles: [
              testProfile('alpha', {
                providers: [
                  { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
                  { provider: 'ollama', model: 'qwen3:14b', enabled: false, default: false },
                ],
              }),
            ],
          }),
          createProfile: vi.fn(),
          updateProfile: vi.fn(),
        }}
      />,
    );

    expect(await screen.findByRole('button', { name: 'Choose model: qwen3:8b · Default' })).toBeInTheDocument();
    expect(screen.queryByText('qwen3:14b')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Models' }));
    expect(screen.getByText('qwen3:14b')).toBeInTheDocument();
    expect(screen.getByText('Disabled')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Back to chat' }));
    expect(screen.queryByText('qwen3:14b')).not.toBeInTheDocument();
  });

  it('filters settings profiles by typing ahead', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['alpha-provider', 'beta-provider', 'zeta-provider'],
            profiles: [testProfile('alpha'), testProfile('beta'), testProfile('zeta')],
            configured_profiles: [testProfile('alpha'), testProfile('beta'), testProfile('zeta')],
          }),
          listProviders: vi.fn().mockResolvedValue([]),
          createProfile: vi.fn(),
          updateProfile: vi.fn(),
          deleteProfile: vi.fn(),
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    const profile = await screen.findByRole('combobox', { name: 'Profile' });
    await user.clear(profile);
    await user.type(profile, 'ze');

    expect(screen.getByRole('option', { name: 'zeta' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'alpha' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'beta' })).not.toBeInTheDocument();

    await user.keyboard('{Enter}');
    expect(profile).toHaveValue('zeta');
    expect(screen.getByLabelText('Name')).toHaveValue('zeta');
  });

  it('restores the selected profile text when typeahead editing is cancelled or blurred', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['alpha-provider', 'zeta-provider'],
            profiles: [testProfile('alpha'), testProfile('zeta')],
            configured_profiles: [testProfile('alpha'), testProfile('zeta')],
          }),
          createProfile: vi.fn(),
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    const profile = await screen.findByRole('combobox', { name: 'Profile' });
    await user.clear(profile);
    await user.type(profile, 'ze');
    await user.keyboard('{Escape}');
    expect(profile).toHaveValue('alpha');

    await user.clear(profile);
    await user.type(profile, 'ze');
    await user.tab();
    expect(profile).toHaveValue('alpha');
  });

  it('offers custom catalog provider identifiers in the models provider typeahead', async () => {
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['ollama', 'unused-custom'],
            profiles: [
              testProfile('alpha', {
                providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
              }),
            ],
            configured_profiles: [
              testProfile('alpha', {
                providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
              }),
            ],
          }),
          createProfile: vi.fn(),
          updateProfile: vi.fn(),
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Models' }));
    const provider = screen.getByRole('combobox', { name: 'Provider' });
    await user.click(provider);

    expect(screen.getByRole('option', { name: 'unused-custom' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'anthropic' })).not.toBeInTheDocument();
  });

  it('preserves model context windows when editing profile identity and skills', async () => {
    const profile = testProfile('alpha', { providers: [
      { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
      { provider: 'ollama', model: 'other', enabled: false, default: false },
    ] });
    const updateProfile = vi.fn().mockImplementation(async (_name: string, next: Profile) => next);
    const user = userEvent.setup();
    render(<App client={{ respond: vi.fn(), updateProfile,
      listProfiles: vi.fn().mockResolvedValue({ default_profile: 'alpha', provider_ids: ['ollama'], profiles: [profile], configured_profiles: [profile] }),
    }} />);
    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('button', { name: 'Models' }));
    await user.type(screen.getByLabelText('Context window for qwen3:8b'), '32768');
    await user.tab();
    expect(updateProfile).toHaveBeenLastCalledWith('alpha', expect.objectContaining({
      providers: [
        { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true, context_window: 32768 },
        { provider: 'ollama', model: 'other', enabled: false, default: false },
      ],
    }));
    await user.click(screen.getByRole('button', { name: 'Profiles' }));
    await user.clear(screen.getByLabelText('Name'));
    await user.type(screen.getByLabelText('Name'), 'renamed');
    await user.type(screen.getByLabelText('Skills'), './skills/review');
    await user.click(screen.getByRole('button', { name: 'Save profile' }));
    expect(updateProfile).toHaveBeenLastCalledWith('alpha', expect.objectContaining({
      name: 'renamed', active_skills: ['./skills/review'], providers: [
        { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true, context_window: 32768 },
        { provider: 'ollama', model: 'other', enabled: false, default: false },
      ],
    }));
  });

  it('adds modifies and deletes profiles from settings', async () => {
    const createProfile = vi.fn().mockImplementation(async (profile: Profile) => profile);
    const updateProfile = vi.fn().mockImplementation(async (_name: string, profile: Profile) => profile);
    const deleteProfile = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue({
            default_profile: 'alpha',
            provider_ids: ['anthropic', 'ollama', 'openai'],
            profiles: [
              testProfile('alpha', {
                providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
              }),
            ],
            configured_profiles: [
              testProfile('alpha', {
                providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
              }),
            ],
          }),
          listProviders: vi.fn().mockResolvedValue([]),
          createProfile,
          updateProfile,
          deleteProfile,
        }}
      />,
    );

    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    await user.click(await screen.findByRole('button', { name: 'Add profile' }));
    await user.type(screen.getByLabelText('Name'), 'work');
    await user.type(screen.getByLabelText('Skills'), 'code-review{Enter}./skills/rust');
    await user.click(screen.getByRole('button', { name: 'Save profile' }));

    expect(createProfile).toHaveBeenCalledWith({
      name: 'work',
      providers: [{ provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true }],
      active_skills: ['code-review', './skills/rust'],
      mcp_servers: [],
      capabilities: [],
      default_project_directory: '.',
      projects: [],
      subagents: [],
    });
    expect(await screen.findByRole('combobox', { name: 'Profile' })).toHaveValue('work');

    expect(screen.getByLabelText('Skills')).toHaveValue('code-review\n./skills/rust');
    await user.clear(screen.getByLabelText('Name'));
    await user.type(screen.getByLabelText('Name'), 'renamed-work');
    await user.click(screen.getByRole('button', { name: 'Save profile' }));
    expect(updateProfile).toHaveBeenCalledWith('work', {
      name: 'renamed-work',
      providers: [{ provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true }],
      active_skills: ['code-review', './skills/rust'],
      mcp_servers: [],
      capabilities: [],
      default_project_directory: '.',
      projects: [],
      subagents: [],
    });

    await user.click(screen.getByRole('button', { name: 'Delete profile' }));
    expect(deleteProfile).toHaveBeenCalledWith('renamed-work');
    expect(await screen.findByRole('combobox', { name: 'Profile' })).toHaveValue('alpha');
    expect(screen.getByLabelText('Skills')).toHaveValue('');
  });
});

it('selects chat provider, model and thinking without changing history or profile defaults', async () => {
  const user = userEvent.setup();
  const profile = testProfile('local', { providers: [
    { provider: 'local', model: 'small', default: true },
    { provider: 'cloud', model: 'fast' },
    { provider: 'cloud', model: 'deep' },
    { provider: 'cloud', model: 'disabled', enabled: false },
  ] });
  let finish: ((value: { message: { role: 'assistant'; content: string } }) => void) | undefined;
  const respond = vi.fn().mockResolvedValueOnce({ message: { role: 'assistant', content: 'First answer' } })
    .mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const updateProfile = vi.fn();
  render(<App client={{ respond, updateProfile, listProfiles: async () => ({ default_profile: 'local', provider_ids: ['local', 'cloud'], profiles: [profile], configured_profiles: [profile] }) }} />);
  const picker = await screen.findByRole('button', { name: /^Choose model:/ });
  await user.type(screen.getByLabelText('Message Rynna'), 'First');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  await screen.findByText('First answer');
  await user.click(picker);
  expect(screen.queryByRole('button', { name: 'disabled' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'deep' }));
  await user.click(picker);
  await user.click(screen.getByRole('radio', { name: 'High' }));
  await user.keyboard('{Escape}');
  await user.type(screen.getByLabelText('Message Rynna'), 'Continue');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(respond.mock.calls[1]![0]).toMatchObject({
    profile: 'local', selection: { provider: 'cloud', model: 'deep', thinking: 'high' },
    history: [{ role: 'user', content: 'First' }, { role: 'assistant', content: 'First answer' }],
  });
  expect(picker).toBeDisabled();
  await act(async () => finish?.({ message: { role: 'assistant', content: 'Second answer' } }));
  await user.click(picker);
  await user.click(screen.getByRole('button', { name: 'fast' }));
  expect(picker).toHaveTextContent('fast· Default');
  await user.click(picker);
  await user.click(screen.getByRole('button', { name: /Profile default/ }));
  expect(picker).toHaveTextContent('small· Default');
  expect(updateProfile).not.toHaveBeenCalled();
});

describe('Stop processing', () => {
  it('keeps partial output, cancels processing, saves it and allows another prompt', async () => {
    let signal: AbortSignal | undefined;
    let lateDelta: (() => void) | undefined;
    const client: AgentClient = { respond: vi.fn((_request, onDelta, abort) => {
      signal = abort;
      onDelta?.({ kind: 'content', content: 'Partial answer' });
      lateDelta = () => onDelta?.({ kind: 'content', content: ' unwanted late text' });
      return new Promise<never>((_, reject) => abort!.addEventListener('abort', () => reject(abort!.reason), { once: true }));
    }) };
    const user = userEvent.setup();
    render(<App client={client} />);
    expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Do some work');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    await user.click(screen.getByRole('button', { name: 'Stop' }));
    expect(signal?.aborted).toBe(true);
    expect(await screen.findByText('Response stopped.')).toBeInTheDocument();
    expect(screen.getByText('Partial answer')).toBeInTheDocument();
    act(() => lateDelta?.());
    expect(screen.queryByText(/unwanted late text/)).not.toBeInTheDocument();
    expect(readSessions()[0]?.messages).toEqual([{ role: 'user', content: 'Do some work' }, { role: 'assistant', content: 'Partial answer' }]);
    await user.type(screen.getByLabelText('Message Rynna'), 'Next task');
    expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled();
  });

  it('shows Stop before the first token arrives and aborts on unmount', async () => {
    let signal: AbortSignal | undefined;
    const client: AgentClient = { respond: vi.fn((_request, _onDelta, abort) => { signal = abort; return new Promise<never>(() => {}); }) };
    const user = userEvent.setup();
    const view = render(<App client={client} />);
    await user.type(screen.getByLabelText('Message Rynna'), 'Think');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(screen.getByRole('button', { name: 'Stop' })).toBeEnabled();
    view.unmount();
    expect(signal?.aborted).toBe(true);
  });
});

it('keeps new conversations and workflow selections unsaved until submission', async () => {
  const user = userEvent.setup();
  const sessionTitle = vi.fn();
  const view = render(<App client={workflowClient({ sessionTitle })} />);
  await screen.findByRole('option', { name: 'Test workflow' });
  await user.type(screen.getByLabelText('Message Rynna'), 'Unsent draft');
  await user.selectOptions(screen.getByRole('combobox', { name: 'Workflow' }), 'workflow');
  await user.type(screen.getByLabelText('Goal'), 'Unsubmitted goal');
  expect(readSessions()).toEqual([]);
  await user.selectOptions(screen.getByRole('combobox', { name: 'Workflow' }), '');
  await user.click(screen.getByRole('button', { name: 'New session' }));
  expect(readSessions()).toEqual([]);
  expect(sessionTitle).not.toHaveBeenCalled();
  view.unmount();
  render(<App client={workflowClient()} />);
  expect(readSessions()).toEqual([]);
});

it('upgrades the first submission title in the background only once', async () => {
  const user = userEvent.setup();
  let finish!: (name: string) => void;
  const sessionTitle = vi.fn(() => new Promise<string>(resolve => { finish = resolve; }));
  render(<App client={{ respond: vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Done' } }), sessionTitle }} />);
  await user.type(screen.getByLabelText('Message Rynna'), 'Please review my Rust code');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  await screen.findByRole('button', { name: 'Please review my Rust code' });
  expect(screen.getByRole('button', { name: 'New session' })).toBeEnabled();
  await act(async () => finish('Rust Code Review'));
  expect(screen.getByRole('button', { name: 'Rust Code Review' })).toBeInTheDocument();
  await user.type(screen.getByLabelText('Message Rynna'), 'Continue');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(sessionTitle).toHaveBeenCalledTimes(1);
  expect(sessionTitle).toHaveBeenCalledWith({ prompt: 'Please review my Rust code', profile: undefined, selection: undefined });
  expect(readSessions()[0]?.name).toBe('Rust Code Review');
});

it.each(['rename', 'remote rename', 'delete', 'failure', 'navigate'])('handles a late session title after %s', async outcome => {
  const user = userEvent.setup();
  let finish!: (name: string) => void;
  let fail!: (error: Error) => void;
  const sessionTitle = vi.fn(() => new Promise<string>((resolve, reject) => { finish = resolve; fail = reject; }));
  render(<App client={{ respond: vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Done' } }), sessionTitle }} />);
  await user.type(screen.getByLabelText('Message Rynna'), 'Opening submission');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  await screen.findByRole('button', { name: 'Opening submission' });
  if (outcome === 'rename') {
    await user.type(screen.getByLabelText('Message Rynna'), '/title My chosen name');
    await user.keyboard('{Enter}');
  } else if (outcome === 'remote rename') {
    writeSessions(readSessions().map(session => ({ ...session, name: 'My chosen name', name_source: 'user' })));
  } else if (outcome === 'delete') {
    const id = readSessions()[0]!.id;
    await act(async () => {
      deleteSession(id, readSessions());
      window.dispatchEvent(new StorageEvent('storage', { key: `rynna-deleted-session-v1:${id}`, newValue: 'true' }));
    });
  } else if (outcome === 'navigate') {
    await user.click(screen.getByRole('button', { name: 'New session' }));
  }
  await act(async () => outcome === 'failure' ? fail(new Error('offline')) : finish('Generated name'));
  expect(readSessions().map(s => s.name)).toEqual(outcome === 'delete' ? [] : [outcome.includes('rename') ? 'My chosen name' : outcome === 'failure' ? 'Opening submission' : 'Generated name']);
  if (outcome === 'navigate') expect(screen.getByText('What should we work through?')).toBeInTheDocument();
});

it('persists and names a workflow only after its goal is submitted', async () => {
  const user = userEvent.setup();
  const sessionTitle = vi.fn().mockResolvedValue('Implement Rust Changes');
  const startWorkflow = vi.fn(async (start: WorkflowRun['start']) => ({ ...workflowRun(start.session_id, 'completed'), start }));
  render(<App client={workflowClient({ startWorkflow, sessionTitle })} />);
  await screen.findByRole('option', { name: 'Test workflow' });
  await user.selectOptions(screen.getByRole('combobox', { name: 'Workflow' }), 'workflow');
  expect(readSessions()).toEqual([]);
  await user.type(screen.getByLabelText('Goal'), 'Implement the Rust changes');
  await user.type(screen.getByLabelText('Success criteria · one per line'), 'Tests pass');
  await user.click(screen.getByRole('button', { name: 'Start workflow' }));
  await screen.findByRole('button', { name: 'Implement Rust Changes' });
  expect(readSessions()).toHaveLength(1);
  expect(readSessions()[0]).toMatchObject({ workflow_id: 'workflow', messages: [{ role: 'user', content: 'Implement the Rust changes' }] });
  expect(sessionTitle).toHaveBeenCalledWith({ prompt: 'Implement the Rust changes', profile: 'work', selection: startWorkflow.mock.calls[0]![0].selection });
});

it('preserves another window’s newer session data when the generated title arrives', async () => {
  const user = userEvent.setup();
  let finish!: (name: string) => void;
  render(<App client={{
    respond: vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Done' } }),
    sessionTitle: () => new Promise(resolve => { finish = resolve; }),
  }} />);
  await user.type(screen.getByLabelText('Message Rynna'), 'Opening submission');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  await screen.findByRole('button', { name: 'Opening submission' });
  const stored = readSessions()[0]!;
  const updated = { ...stored, project: 'renamed-project', updated_at: '2099-01-01T00:00:00.000Z',
    messages: [...stored.messages, { role: 'user' as const, content: 'From another window' }, { role: 'assistant' as const, content: 'New answer' }] };
  writeSessions([updated]);
  await act(async () => finish('Generated title'));
  expect(readSessions()).toEqual([{ ...updated, name: 'Generated title', name_source: 'llm' }]);
});

it('shows the workflow goal once when polling and start resolve before a render', async () => {
  const user = userEvent.setup();
  let finishPoll!: (runs: WorkflowRun[]) => void;
  let finishStart!: (run: WorkflowRun) => void;
  const listWorkflowRuns = vi.fn(() => new Promise<WorkflowRun[]>(resolve => { finishPoll = resolve; }));
  const startWorkflow = vi.fn((_start: WorkflowRun['start']) => new Promise<WorkflowRun>(resolve => { finishStart = resolve; }));
  render(<App client={workflowClient({ listWorkflowRuns, startWorkflow })} />);
  await screen.findByRole('option', { name: 'Test workflow' });
  await user.selectOptions(screen.getByRole('combobox', { name: 'Workflow' }), 'workflow');
  await user.type(screen.getByLabelText('Goal'), 'Implement the Rust changes');
  await user.type(screen.getByLabelText('Success criteria · one per line'), 'Tests pass');
  await user.click(screen.getByRole('button', { name: 'Start workflow' }));
  const start = startWorkflow.mock.calls[0]![0];
  const run = { ...workflowRun(start.session_id, 'running'), start, events: [{ id: 1, step_id: 'implement', content: 'Working on it' }] };
  await act(async () => { finishPoll([run]); finishStart(run); });
  expect(within(screen.getByRole('log')).getAllByText(start.goal)).toHaveLength(1);
  expect(within(screen.getByRole('log')).getAllByText('Working on it')).toHaveLength(1);
  expect(readSessions()[0]!.messages).toEqual([{ role: 'user', content: start.goal }, { role: 'assistant', content: 'Working on it' }]);
});
