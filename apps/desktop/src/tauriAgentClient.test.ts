import type { CompletionDelta } from '@rynna/ui';
import { describe, expect, it, vi } from 'vitest';

import { TauriAgentClient } from './tauriAgentClient';

describe('TauriAgentClient', () => {
  it('accepts MLX provider settings in list and save responses', async () => {
    const provider = { kind: 'mlx' as const, api_base: 'http://localhost:8000/v1' };
    const transport = vi.fn().mockResolvedValueOnce([provider]).mockResolvedValueOnce(provider).mockResolvedValueOnce(provider);
    const client = new TauriAgentClient(transport);
    await expect(client.listProviders('work')).resolves.toEqual([provider]);
    await expect(client.createProvider(provider, 'work')).resolves.toEqual(provider);
    await expect(client.updateProvider(provider, 'work')).resolves.toEqual(provider);
  });

  it('loads and saves MCP settings through narrow desktop commands', async () => {
    const invoke = vi.fn().mockResolvedValue({ mcpServers: {} });
    const client = new TauriAgentClient(invoke);
    expect(await client.getMcpSettings('work profile')).toEqual({ mcpServers: {} });
    expect(invoke).toHaveBeenLastCalledWith('get_mcp_settings', { profile: 'work profile' });
    await client.saveMcpSettings({ mcpServers: {} }, 'work profile');
    expect(invoke).toHaveBeenLastCalledWith('save_mcp_settings', { settings: { mcpServers: {} }, profile: 'work profile' });
    invoke.mockResolvedValueOnce({ kind: 'hindsight' });
    await expect(client.getMcpSettings('work profile')).rejects.toThrow('invalid MCP settings');
  });

  it('loads and saves memory settings through narrow desktop commands', async () => {
    const invoke = vi.fn().mockResolvedValue({ kind: 'none' });
    const client = new TauriAgentClient(invoke);
    expect(await client.getMemorySettings('work profile')).toEqual({ kind: 'none' });
    expect(invoke).toHaveBeenLastCalledWith('get_memory_settings', { profile: 'work profile' });
    await client.saveMemorySettings({ kind: 'none' }, 'work profile');
    expect(invoke).toHaveBeenLastCalledWith('save_memory_settings', { settings: { kind: 'none' }, profile: 'work profile' });
    invoke.mockResolvedValueOnce({ kind: 'hindsight' });
    await expect(client.getMemorySettings('work profile')).rejects.toThrow('invalid memory settings');
  });

  it('invokes the narrow desktop response command', async () => {
    const invoke = vi.fn().mockResolvedValue({
      message: { role: 'assistant', content: 'From Tauri.' },
    });
    const client = new TauriAgentClient(invoke);
    const request = { prompt: 'Hello', history: [], selection: { provider: 'openai', model: 'enabled-model', thinking: 'high' as const } };

    const response = await client.respond(request);

    expect(invoke).toHaveBeenCalledWith('respond', { request });
    expect(response.message.content).toBe('From Tauri.');
  });

  it('streams typed thinking and content through a narrow Tauri channel', async () => {
    const channel = {
      onmessage: null as ((message: CompletionDelta) => void) | null,
    };
    const invoke = vi.fn().mockImplementation(async (_command, args) => {
      channel.onmessage?.({ kind: 'thinking', content: 'Inspect' });
      channel.onmessage?.({ kind: 'tool_started', call: { id: 'cmd', name: 'run_command', arguments: { program: 'pwd' } } });
      channel.onmessage?.({ kind: 'tool_finished', id: 'cmd' });
      channel.onmessage?.({ kind: 'content', content: 'Answer' });
      expect(args.onEvent).toBe(channel);
      return { message: { role: 'assistant', content: 'Answer' } };
    });
    const client = new TauriAgentClient(invoke, () => channel);
    const deltas: unknown[] = [];
    const request = { prompt: 'Hello', history: [], selection: { provider: 'openai', model: 'enabled-model', thinking: 'high' as const } };

    const response = await client.respond(request, (delta) => deltas.push(delta));

    expect(invoke).toHaveBeenCalledWith('respond_stream', { request, onEvent: channel });
    expect(deltas).toEqual([
      { kind: 'thinking', content: 'Inspect' },
      { kind: 'tool_started', call: { id: 'cmd', name: 'run_command', arguments: { program: 'pwd' } } },
      { kind: 'tool_finished', id: 'cmd' },
      { kind: 'content', content: 'Answer' },
    ]);
    expect(response.message.content).toBe('Answer');
  });

  it('loads profiles through the narrow desktop profiles command', async () => {
    const invoke = vi.fn().mockResolvedValue({
      default_profile: 'local',
      provider_ids: ['ollama', 'unused-custom'],
      profiles: [
        {
          name: 'local',
          providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
          active_skills: [],
          mcp_servers: ['filesystem'],
          capabilities: ['workspace'],
          default_project_directory: '.',
          projects: [],
          subagents: [],
        },
      ],
      configured_profiles: [
        {
          name: 'local',
          providers: [
            { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
            { provider: 'ollama', model: 'qwen3:14b', enabled: false, default: false },
          ],
          active_skills: [],
          mcp_servers: ['filesystem'],
          capabilities: ['workspace'],
          default_project_directory: '.',
          projects: [],
          subagents: [],
        },
      ],
    });
    const client = new TauriAgentClient(invoke);

    const profiles = await client.listProfiles();

    expect(invoke).toHaveBeenCalledWith('profiles', {});
    expect(profiles.default_profile).toBe('local');
    expect(profiles.provider_ids).toEqual(['ollama', 'unused-custom']);
    expect(profiles.profiles[0]!.mcp_servers).toEqual(['filesystem']);
    expect(profiles.configured_profiles[0]!.providers[1]!.enabled).toBe(false);
  });

  it('rejects profile metadata that omits configured profiles', async () => {
    const invoke = vi.fn().mockResolvedValue({
      default_profile: 'local',
      provider_ids: ['ollama'],
      profiles: [
        {
          name: 'local',
          providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
          active_skills: [],
          mcp_servers: [],
          capabilities: [],
        },
      ],
    });

    await expect(new TauriAgentClient(invoke).listProfiles()).rejects.toThrow(
      'invalid profile data',
    );
  });

  it('uses narrow commands for profile CRUD', async () => {
    const profile = {
      name: 'work',
      providers: [{ provider: 'openai', model: 'gpt-5' }],
      active_skills: [],
      mcp_servers: [],
      capabilities: [],
      default_project_directory: '.',
      projects: [],
      subagents: [{ name: 'reviewer', description: 'Review code', instructions: 'Find bugs' }],
    };
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(profile)
      .mockResolvedValueOnce({
        ...profile,
        providers: [{ provider: 'openai', model: 'gpt-5.2' }],
      })
      .mockResolvedValueOnce(undefined);
    const client = new TauriAgentClient(invoke);

    await client.createProfile(profile);
    await client.updateProfile('work', {
      ...profile,
      providers: [{ provider: 'openai', model: 'gpt-5.2' }],
    });
    await client.deleteProfile('work');

    expect(invoke).toHaveBeenNthCalledWith(1, 'create_profile', { profile });
    expect(invoke).toHaveBeenNthCalledWith(2, 'update_profile', {
      name: 'work',
      profile: {
        ...profile,
        providers: [{ provider: 'openai', model: 'gpt-5.2' }],
      },
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'delete_profile', { name: 'work' });
  });

  it('uses narrow commands for OpenAI account status and login', async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce({ connected: false, method: null })
      .mockResolvedValueOnce({ connected: true, method: 'chatgpt', plan: 'plus' })
      .mockResolvedValueOnce({ connected: true, method: 'api_key' });
    const client = new TauriAgentClient(invoke);

    await expect(client.getOpenAiAccount()).resolves.toEqual({ connected: false, method: null });
    await expect(client.getExistingOpenAiAccount()).resolves.toEqual({
      connected: true,
      method: 'chatgpt',
      plan: 'plus',
    });
    await expect(
      client.connectOpenAi({ method: 'api_key', api_key: 'sk-secret' }),
    ).resolves.toEqual({ connected: true, method: 'api_key' });

    expect(invoke).toHaveBeenNthCalledWith(1, 'openai_account', {});
    expect(invoke).toHaveBeenNthCalledWith(2, 'existing_openai_account', {});
    expect(invoke).toHaveBeenNthCalledWith(3, 'connect_openai', {
      request: { method: 'api_key', api_key: 'sk-secret' },
    });
  });

  it('uses narrow commands for provider settings CRUD', async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce([{ kind: 'openrouter' }])
      .mockResolvedValueOnce({ kind: 'openai', authentication: 'chatgpt' })
      .mockResolvedValueOnce({ kind: 'openai', authentication: 'api_key' })
      .mockResolvedValueOnce(undefined);
    const client = new TauriAgentClient(invoke);

    await expect(client.listProviders('work')).resolves.toEqual([{ kind: 'openrouter' }]);
    await client.createProvider({ kind: 'openai', authentication: 'chatgpt' }, 'work');
    await client.updateProvider({ kind: 'openai', authentication: 'api_key', api_key: 'sk-secret' }, 'work');
    await client.deleteProvider('openai', 'work');

    expect(invoke).toHaveBeenNthCalledWith(1, 'list_providers', { profile: 'work' });
    expect(invoke).toHaveBeenNthCalledWith(2, 'create_provider', {
      profile: 'work',
      provider: { kind: 'openai', authentication: 'chatgpt' },
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'update_provider', {
      profile: 'work',
      provider: { kind: 'openai', authentication: 'api_key', api_key: 'sk-secret' },
    });
    expect(invoke).toHaveBeenNthCalledWith(4, 'delete_provider', {
      kind: 'openai',
      profile: 'work',
    });
  });
});

it('uses the same scoped and revisioned workflow contracts through IPC', async () => {
  const invoke = vi.fn().mockResolvedValue([]);
  const client = new TauriAgentClient(invoke);
  await client.listWorkflowRuns('work profile', 'session');
  expect(invoke).toHaveBeenLastCalledWith('list_workflow_runs', { profile: 'work profile', sessionId: 'session' });
  const control = { profile: 'work', session_id: 'session', expected_revision: 7, action: 'resume' as const, acknowledge_uncertain: true };
  await client.controlWorkflow('run', control);
  expect(invoke).toHaveBeenLastCalledWith('control_workflow', { id: 'run', request: control });
  invoke.mockRejectedValueOnce(new Error('stale revision'));
  await expect(client.controlWorkflow('run', control)).rejects.toThrow('stale revision');
});

it.each([true, false])('cancels a desktop response when stopped before started=%s', async (early) => {
  const channel = { onmessage: null as ((message: unknown) => void) | null };
  let finish: (reason: unknown) => void = () => {};
  const invoke = vi.fn((command: string) => command === 'cancel_response'
    ? Promise.resolve().then(() => { finish('Response stopped'); })
    : new Promise((_, reject) => { finish = reject; }));
  const client = new TauriAgentClient(invoke, () => channel);
  const abort = new AbortController();
  const delta = vi.fn();
  const pending = client.respond({ prompt: 'Think', history: [] }, delta, abort.signal);
  const rejected = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
  if (early) abort.abort();
  channel.onmessage?.({ kind: 'started' });
  if (!early) abort.abort();
  channel.onmessage?.({ kind: 'content', content: 'late' });
  await rejected;
  expect(invoke).toHaveBeenCalledWith('cancel_response', { responseId: expect.any(String) });
  expect(delta).not.toHaveBeenCalled();
});

it('does not dispatch an already aborted request', async () => {
  const invoke = vi.fn();
  const controller = new AbortController(); controller.abort();
  await expect(new TauriAgentClient(invoke).respond({ prompt: 'Hi', history: [] }, undefined, controller.signal)).rejects.toMatchObject({ name: 'AbortError' });
  expect(invoke).not.toHaveBeenCalled();
});

it('preserves portable summaries through the desktop context command', async () => {
  const response = { history: [{ role: 'assistant', content: 'Visible answer', provider_context: { provider: 'conversation_summary', state: 'Remember this' } }], size: { current_tokens: 200, max_tokens: 4000 }, compacted: true, limit_known: true };
  const invoke = vi.fn().mockResolvedValue(response);
  const client = new TauriAgentClient(invoke);
  expect(await client.conversationContext({ profile: 'work', history: [], compact: true })).toEqual(response);
  expect(invoke).toHaveBeenCalledWith('conversation_context', { request: { profile: 'work', history: [], compact: true } });
  invoke.mockResolvedValueOnce({ history: [] });
  await expect(client.conversationContext({ history: [] })).rejects.toThrow('invalid context data');
});

it('requests a session title through the dedicated desktop command', async () => {
  const invoke = vi.fn().mockResolvedValue('Rust Code Review');
  const client = new TauriAgentClient(invoke);
  expect(await client.sessionTitle({ prompt: 'Review Rust code', profile: 'work' })).toBe('Rust Code Review');
  expect(invoke).toHaveBeenCalledWith('session_title', { request: { prompt: 'Review Rust code', profile: 'work' } });
  invoke.mockResolvedValueOnce('');
  await expect(client.sessionTitle({ prompt: 'Review' })).rejects.toThrow('invalid session title');
});

it('discovers models for the selected provider and rejects invalid results', async () => {
  const transport = vi.fn().mockResolvedValueOnce(['model-a']).mockResolvedValueOnce([123]);
  const client = new TauriAgentClient(transport);
  await expect(client.listProviderModels('work profile', 'custom/provider')).resolves.toEqual(['model-a']);
  expect(transport).toHaveBeenLastCalledWith('list_provider_models', { profile: 'work profile', provider: 'custom/provider' });
  await expect(client.listProviderModels('work profile', 'custom/provider')).rejects.toThrow('invalid model data');
});
