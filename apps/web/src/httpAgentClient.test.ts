import { describe, expect, it, vi } from 'vitest';

import { HttpAgentClient } from './httpAgentClient';

describe('HttpAgentClient', () => {
  it('accepts MLX provider settings in list and save responses', async () => {
    const provider = { kind: 'mlx' as const, api_base: 'http://localhost:8000/v1' };
    const transport = vi.fn().mockResolvedValueOnce(jsonResponse([provider])).mockResolvedValueOnce(jsonResponse(provider)).mockResolvedValueOnce(jsonResponse(provider));
    const client = new HttpAgentClient(undefined, transport);
    await expect(client.listProviders('work')).resolves.toEqual([provider]);
    await expect(client.createProvider(provider, 'work')).resolves.toEqual(provider);
    await expect(client.updateProvider(provider, 'work')).resolves.toEqual(provider);
  });

  it('loads and saves MCP settings through the HTTP settings endpoint', async () => {
    const fetcher = vi.fn().mockImplementation(async () => new Response(JSON.stringify({ mcpServers: {} })));
    const client = new HttpAgentClient('/custom/v1/respond', fetcher);
    expect(await client.getMcpSettings('work profile')).toEqual({ mcpServers: {} });
    expect(fetcher).toHaveBeenLastCalledWith('/custom/v1/profiles/work%20profile/mcp', { method: 'GET', headers: { accept: 'application/json' } });
    await client.saveMcpSettings({ mcpServers: {} }, 'work profile');
    expect(fetcher).toHaveBeenLastCalledWith('/custom/v1/profiles/work%20profile/mcp', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: '{"mcpServers":{}}' });
    fetcher.mockResolvedValueOnce(new Response('{"kind":"hindsight"}'));
    await expect(client.getMcpSettings('work profile')).rejects.toThrow('invalid MCP settings');
  });

  it('loads and saves memory settings through the HTTP settings endpoint', async () => {
    const fetcher = vi.fn().mockImplementation(async () => new Response(JSON.stringify({ kind: 'none' })));
    const client = new HttpAgentClient('/custom/v1/respond', fetcher);
    expect(await client.getMemorySettings('work profile')).toEqual({ kind: 'none' });
    expect(fetcher).toHaveBeenLastCalledWith('/custom/v1/profiles/work%20profile/memory', { method: 'GET', headers: { accept: 'application/json' } });
    await client.saveMemorySettings({ kind: 'none' }, 'work profile');
    expect(fetcher).toHaveBeenLastCalledWith('/custom/v1/profiles/work%20profile/memory', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: '{"kind":"none"}' });
    fetcher.mockResolvedValueOnce(new Response('{"kind":"hindsight"}'));
    await expect(client.getMemorySettings('work profile')).rejects.toThrow('invalid memory settings');
  });

  it('posts a response request to the Rynna API', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          message: { role: 'assistant', content: 'From the server.' },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    const client = new HttpAgentClient('/v1/respond', fetcher);
    const request = {
      prompt: 'Hello',
      history: [{ role: 'user' as const, content: 'Earlier' }],
    };

    const response = await client.respond(request);

    expect(fetcher).toHaveBeenCalledWith('/v1/respond', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    expect(response.message.content).toBe('From the server.');
  });

  it('streams typed thinking and content events from the Rynna API', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response(
        [
          'data: {"kind":"thinking","content":"Inspect"}\n\n',
          'data: {"kind":"content","content":"Answer"}\n\n',
          'data: {"kind":"done","message":{"role":"assistant","content":"Answer"}}\n\n',
        ].join(''),
        { status: 200, headers: { 'content-type': 'text/event-stream' } },
      ),
    );
    const client = new HttpAgentClient('/v1/respond', fetcher);
    const deltas: unknown[] = [];
    const request = { prompt: 'Hello', history: [], selection: { provider: 'openai', model: 'enabled-model', thinking: 'high' as const } };

    const response = await client.respond(request, (delta) => deltas.push(delta));

    expect(fetcher).toHaveBeenCalledWith('/v1/respond/stream', {
      method: 'POST',
      headers: {
        accept: 'text/event-stream',
        'content-type': 'application/json',
      },
      body: JSON.stringify(request),
    });
    expect(deltas).toEqual([
      { kind: 'thinking', content: 'Inspect' },
      { kind: 'content', content: 'Answer' },
    ]);
    expect(response.message.content).toBe('Answer');
  });

  it('reports an HTTP status when an error response is not JSON', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response('Bad Gateway', {
        status: 502,
        headers: { 'content-type': 'text/plain' },
      }),
    );
    const client = new HttpAgentClient('/v1/respond', fetcher);

    await expect(client.respond({ prompt: 'Hello', history: [] })).rejects.toThrow(
      'Rynna API returned 502',
    );
  });

  it('loads profile metadata from the profiles endpoint', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          default_profile: 'local',
          provider_ids: ['ollama', 'unused-custom'],
          profiles: [
            {
              name: 'local',
              providers: [{ provider: 'ollama', model: 'qwen3:8b' }],
              active_skills: ['rust'],
              mcp_servers: [],
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
              active_skills: ['rust'],
              mcp_servers: [],
              capabilities: ['workspace'],
              default_project_directory: '.',
              projects: [],
              subagents: [],
            },
          ],
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    const client = new HttpAgentClient('/v1/respond', fetcher);

    const profiles = await client.listProfiles();

    expect(fetcher).toHaveBeenCalledWith('/v1/profiles', {
      method: 'GET',
      headers: { accept: 'application/json' },
    });
    expect(profiles.default_profile).toBe('local');
    expect(profiles.provider_ids).toEqual(['ollama', 'unused-custom']);
    expect(profiles.profiles[0]!.active_skills).toEqual(['rust']);
    expect(profiles.configured_profiles[0]!.providers[1]!.enabled).toBe(false);
  });

  it('rejects profile metadata that omits configured profiles', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      jsonResponse({
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
      }),
    );

    await expect(new HttpAgentClient('/v1/respond', fetcher).listProfiles()).rejects.toThrow(
      'invalid profile data',
    );
  });

  it('creates updates and deletes profiles through the profiles API', async () => {
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
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(profile))
      .mockResolvedValueOnce(
        jsonResponse({ ...profile, providers: [{ provider: 'openai', model: 'gpt-5.2' }] }),
      )
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const client = new HttpAgentClient('/v1/respond', fetcher);

    await client.createProfile(profile);
    await client.updateProfile('work', {
      ...profile,
      providers: [{ provider: 'openai', model: 'gpt-5.2' }],
    });
    await client.deleteProfile('work');

    expect(fetcher).toHaveBeenNthCalledWith(1, '/v1/profiles', expect.objectContaining({ method: 'POST' }));
    expect(fetcher).toHaveBeenNthCalledWith(2, '/v1/profiles/work', expect.objectContaining({ method: 'PUT' }));
    expect(fetcher).toHaveBeenNthCalledWith(3, '/v1/profiles/work', expect.objectContaining({ method: 'DELETE' }));
  });

  it('lists and mutates provider settings through the providers API', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse([{ kind: 'openrouter' }]))
      .mockResolvedValueOnce(jsonResponse({ kind: 'ollama', api_base: 'http://localhost:11434/v1' }))
      .mockResolvedValueOnce(jsonResponse({ kind: 'ollama', api_base: 'http://localhost:22434/v1' }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const client = new HttpAgentClient('/v1/respond', fetcher);

    await expect(client.listProviders('work')).resolves.toEqual([{ kind: 'openrouter' }]);
    await client.createProvider({ kind: 'ollama', api_base: 'http://localhost:11434/v1' }, 'work');
    await client.updateProvider({ kind: 'ollama', api_base: 'http://localhost:22434/v1' }, 'work');
    await client.deleteProvider('ollama', 'work');

    expect(fetcher).toHaveBeenNthCalledWith(1, '/v1/profiles/work/providers', expect.objectContaining({ method: 'GET' }));
    expect(fetcher).toHaveBeenNthCalledWith(2, '/v1/profiles/work/providers', expect.objectContaining({ method: 'POST' }));
    expect(fetcher).toHaveBeenNthCalledWith(3, '/v1/profiles/work/providers/ollama', expect.objectContaining({ method: 'PUT' }));
    expect(fetcher).toHaveBeenNthCalledWith(4, '/v1/profiles/work/providers/ollama', expect.objectContaining({ method: 'DELETE' }));
  });

  it('discovers an existing ChatGPT subscription through the providers API', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      jsonResponse({ connected: true, method: 'chatgpt' }),
    );
    const client = new HttpAgentClient('/v1/respond', fetcher);

    await expect(client.getExistingOpenAiAccount()).resolves.toEqual({
      connected: true,
      method: 'chatgpt',
    });
    expect(fetcher).toHaveBeenCalledWith('/v1/providers/openai/existing-account', {
      method: 'GET',
      headers: { accept: 'application/json' },
    });
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

it('scopes workflow reads and controls and accepts successful deletion without JSON', async () => {
  const fetcher = vi.fn().mockResolvedValue(new Response('[]'));
  const client = new HttpAgentClient('/prefix/v1/respond', fetcher);
  await client.listWorkflowRuns('work profile', 'session');
  expect(fetcher).toHaveBeenLastCalledWith('/prefix/v1/workflow-runs?profile=work+profile&session_id=session', expect.objectContaining({ method: 'GET' }));
  const control = { profile: 'work', session_id: 'session', expected_revision: 7, action: 'resume' as const, acknowledge_uncertain: true };
  fetcher.mockResolvedValueOnce(new Response('{}'));
  await client.controlWorkflow('run', control);
  expect(fetcher).toHaveBeenLastCalledWith('/prefix/v1/workflow-runs/run', expect.objectContaining({ method: 'POST', body: JSON.stringify(control) }));
  fetcher.mockResolvedValueOnce(new Response(null, { status: 204 }));
  await expect(client.deleteWorkflow('work', 'custom')).resolves.toBeUndefined();
  fetcher.mockResolvedValueOnce(new Response('{"error":{"message":"stale revision"}}', { status: 409 }));
  await expect(client.controlWorkflow('run', control)).rejects.toThrow('stale revision');
});

it('passes cancellation to streaming fetch even without a delta handler', async () => {
  const controller = new AbortController();
  const fetcher = vi.fn((_url, init) => new Promise<Response>((_, reject) => {
    init.signal.addEventListener('abort', () => reject(init.signal.reason));
  }));
  const client = new HttpAgentClient('/v1/respond', fetcher);
  const pending = client.respond({ prompt: 'Think', history: [] }, undefined, controller.signal);
  const rejected = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
  controller.abort();
  await rejected;
  expect(fetcher).toHaveBeenCalledWith('/v1/respond/stream', expect.objectContaining({ signal: controller.signal }));
});

it('preserves portable summaries and validates context API responses', async () => {
  const history = [{ role: 'assistant', content: 'Visible answer', provider_context: { provider: 'conversation_summary', state: 'Remember this' } }];
  const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({ history, size: { current_tokens: 200, max_tokens: 4000 }, compacted: true, limit_known: true })));
  const client = new HttpAgentClient('/custom/v1/respond', fetcher);
  expect((await client.conversationContext({ history: [], compact: true })).history).toEqual(history);
  expect(fetcher.mock.calls[0]![0]).toBe('/custom/v1/context');
  fetcher.mockResolvedValueOnce(new Response(JSON.stringify({ history, size: { current_tokens: 0, max_tokens: 0 }, compacted: true, limit_known: true })));
  await expect(client.conversationContext({ history: [] })).rejects.toThrow('invalid context data');
});

it('requests a session title independently of the chat endpoint', async () => {
  const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify('Rust Code Review')));
  const client = new HttpAgentClient('/custom/v1/respond', fetcher);
  expect(await client.sessionTitle({ prompt: 'Review Rust code', profile: 'work' })).toBe('Rust Code Review');
  expect(fetcher.mock.calls[0]![0]).toBe('/custom/v1/session-title');
  expect(JSON.parse(fetcher.mock.calls[0]![1].body)).toEqual({ prompt: 'Review Rust code', profile: 'work' });
  fetcher.mockResolvedValueOnce(new Response(JSON.stringify({ message: 'bad' })));
  await expect(client.sessionTitle({ prompt: 'Review' })).rejects.toThrow('invalid session title');
});
