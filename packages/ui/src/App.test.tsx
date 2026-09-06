import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import type { AgentClient, Profile } from './contracts';

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
    ...overrides,
  };
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
    );
    expect(await screen.findByText('Follow the thread.')).toBeInTheDocument();
    expect(within(screen.getByRole('log')).getByText('Help me plan this')).toBeInTheDocument();
    const firstRequest = vi.mocked(client.respond).mock.calls[0]![0];
    expect(firstRequest.session_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    await user.type(screen.getByLabelText('Message Rynna'), 'Continue');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(vi.mocked(client.respond).mock.calls[1]![0].session_id).toBe(firstRequest.session_id);

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

    await screen.findByRole('combobox', { name: 'Project' });
    await user.selectOptions(screen.getByRole('combobox', { name: 'Project' }), 'rynna');
    await user.type(screen.getByLabelText('Message Rynna'), 'Inspect it');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond).toHaveBeenLastCalledWith(expect.objectContaining({
      profile: 'work',
      project: 'rynna',
      session_id: expect.any(String),
    }), expect.any(Function));
    const firstSession = respond.mock.calls[0]![0].session_id;

    await user.selectOptions(screen.getByRole('combobox', { name: 'Project' }), '');
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

    await screen.findByRole('combobox', { name: 'Project' });
    await user.selectOptions(screen.getByRole('combobox', { name: 'Project' }), 'rynna');
    await user.type(screen.getByLabelText('Message Rynna'), 'Review Rynna changes');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByRole('button', { name: 'Review Rynna changes' })).toHaveAttribute('aria-current', 'page');
    const firstSession = respond.mock.calls[0]![0].session_id;

    await user.selectOptions(screen.getByRole('combobox', { name: 'Project' }), '');
    expect(screen.queryByText('The project is healthy.')).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Message Rynna'), 'Plan something else');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByRole('button', { name: 'Plan something else' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Review Rynna changes' }));
    expect(screen.getByText('The project is healthy.')).toBeInTheDocument();
    expect(screen.queryByText('A fresh answer.')).not.toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: 'Project' })).toHaveValue('rynna');

    await user.type(screen.getByLabelText('Message Rynna'), 'Continue the review');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(respond).toHaveBeenLastCalledWith(expect.objectContaining({
      session_id: firstSession,
      project: 'rynna',
      history: [
        { role: 'user', content: 'Review Rynna changes' },
        { role: 'assistant', content: 'The project is healthy.' },
      ],
    }), expect.any(Function));
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
          },
          {
            name: 'work',
            providers: [{ provider: 'openai', model: 'gpt-5' }],
            active_skills: ['github'],
            mcp_servers: ['github'],
            capabilities: [],
            default_project_directory: '.',
            projects: [],
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
          },
          {
            name: 'work',
            providers: [{ provider: 'openai', model: 'gpt-5' }],
            active_skills: ['github'],
            mcp_servers: ['github'],
            capabilities: [],
            default_project_directory: '.',
            projects: [],
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
    expect(within(runtimeSummary).getByText('qwen3:8b')).toBeInTheDocument();
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
