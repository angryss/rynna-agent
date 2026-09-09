import { isContextResponse, type ContextRequest, type ContextResponse } from '@rynna/ui';
import type { Workflow, WorkflowMetadata, WorkflowStart, WorkflowRun, WorkflowControl } from '@rynna/ui';
import { isMcpSettings, isMemorySettings } from '@rynna/ui';
import type {
  AgentClient,
  SessionTitleRequest,
  McpSettings,
  MemorySettings,
  MemorySettingsInput,
  CompletionDelta,
  CompletionDeltaHandler,
  ConfiguredProvider,
  OpenAiAccount,
  Profile,
  ProfileCatalog,
  ProviderInput,
  RespondRequest,
  RespondResponse,
} from '@rynna/ui';

type Fetcher = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export class HttpAgentClient implements AgentClient {
  private readonly endpoint: string;
  private readonly profilesEndpoint: string;
  private readonly providersEndpoint: string;
  private readonly fetcher: Fetcher;

  constructor(
    endpoint = defaultEndpoint(),
    fetcher: Fetcher = globalThis.fetch.bind(globalThis),
    profilesEndpoint = endpoint.replace(/\/respond$/, '/profiles'),
  ) {
    this.endpoint = endpoint;
    this.profilesEndpoint = profilesEndpoint;
    this.providersEndpoint = endpoint.replace(/\/respond$/, '/providers');
    this.fetcher = fetcher;
  }

  async sessionTitle(request: SessionTitleRequest): Promise<string> {
    const title = await this.providerRequest(this.endpoint.replace(/\/respond$/, '/session-title'), 'POST', request);
    if (typeof title !== 'string' || !title.trim() || title.length > 200) throw new Error('Rynna returned an invalid session title');
    return title;
  }

  async conversationContext(request: ContextRequest): Promise<ContextResponse> {
    const response = await this.providerRequest(this.endpoint.replace(/\/respond$/, '/context'), 'POST', request);
    if (!isContextResponse(response)) throw new Error('Rynna returned invalid context data');
    return response;
  }

  async listWorkflows(profile: string): Promise<WorkflowMetadata[]> { return await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/workflows`, 'GET') as WorkflowMetadata[]; }
  async readWorkflow(profile: string, id: string): Promise<Workflow> { return await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/workflows/${encodeURIComponent(id)}`, 'GET') as Workflow; }
  async saveWorkflow(profile: string, workflow: Workflow): Promise<Workflow> { return await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/workflows`, 'POST', workflow) as Workflow; }
  async deleteWorkflow(profile: string, id: string): Promise<void> { await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/workflows/${encodeURIComponent(id)}`, 'DELETE'); }
  async startWorkflow(request: WorkflowStart): Promise<WorkflowRun> { return await this.providerRequest(this.endpoint.replace(/\/respond$/, '/workflow-runs'), 'POST', request) as WorkflowRun; }
  async readWorkflowRun(id: string, profile: string, session: string): Promise<WorkflowRun> { return await this.providerRequest(`${this.endpoint.replace(/\/respond$/, '/workflow-runs')}/${encodeURIComponent(id)}?${new URLSearchParams({ profile, session_id: session })}`, 'GET') as WorkflowRun; }
  async listWorkflowRuns(profile: string, session: string): Promise<WorkflowRun[]> { return await this.providerRequest(`${this.endpoint.replace(/\/respond$/, '/workflow-runs')}?${new URLSearchParams({ profile, session_id: session })}`, 'GET') as WorkflowRun[]; }
  async controlWorkflow(id: string, request: WorkflowControl): Promise<WorkflowRun> { return await this.providerRequest(`${this.endpoint.replace(/\/respond$/, '/workflow-runs')}/${encodeURIComponent(id)}`, 'POST', request) as WorkflowRun; }

  async getMcpSettings(profile: string): Promise<McpSettings> {
    const body = await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/mcp`, 'GET');
    if (!isMcpSettings(body)) throw new Error('Rynna API returned invalid MCP settings');
    return body;
  }

  async saveMcpSettings(settings: McpSettings, profile: string): Promise<McpSettings> {
    const body = await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/mcp`, 'PUT', settings);
    if (!isMcpSettings(body)) throw new Error('Rynna API returned invalid MCP settings');
    return body;
  }

  async getMemorySettings(profile: string): Promise<MemorySettings> {
    const body = await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/memory`, 'GET');
    if (!isMemorySettings(body)) throw new Error('Rynna API returned invalid memory settings');
    return body;
  }

  async saveMemorySettings(settings: MemorySettingsInput, profile: string): Promise<MemorySettings> {
    const body = await this.providerRequest(`${this.profilesEndpoint}/${encodeURIComponent(profile)}/memory`, 'PUT', settings);
    if (!isMemorySettings(body)) throw new Error('Rynna API returned invalid memory settings');
    return body;
  }

  async listProviders(profile: string): Promise<ConfiguredProvider[]> {
    const body = await this.providerRequest(this.profileProvidersEndpoint(profile), 'GET');
    if (!Array.isArray(body) || !body.every(isConfiguredProvider)) {
      throw new Error('Rynna API returned invalid provider data');
    }
    return body;
  }

  async getExistingOpenAiAccount(): Promise<OpenAiAccount> {
    const body = await this.providerRequest(
      `${this.providersEndpoint}/openai/existing-account`,
      'GET',
    );
    if (!isOpenAiAccount(body)) {
      throw new Error('Rynna API returned invalid OpenAI account data');
    }
    return body;
  }

  async createProvider(provider: ProviderInput, profile: string): Promise<ConfiguredProvider> {
    return this.savedProvider(this.profileProvidersEndpoint(profile), 'POST', provider);
  }

  async updateProvider(provider: ProviderInput, profile: string): Promise<ConfiguredProvider> {
    return this.savedProvider(
      `${this.profileProvidersEndpoint(profile)}/${provider.kind}`,
      'PUT',
      provider,
    );
  }

  async deleteProvider(kind: ConfiguredProvider['kind'], profile: string): Promise<void> {
    const response = await this.fetcher(`${this.profileProvidersEndpoint(profile)}/${kind}`, {
      method: 'DELETE',
    });
    if (!response.ok) throw new Error(`Rynna API returned ${response.status}`);
  }

  private profileProvidersEndpoint(profile: string): string {
    return `${this.profilesEndpoint}/${encodeURIComponent(profile)}/providers`;
  }

  private async savedProvider(
    endpoint: string,
    method: 'POST' | 'PUT',
    provider: ProviderInput,
  ): Promise<ConfiguredProvider> {
    const body = await this.providerRequest(endpoint, method, provider);
    if (!isConfiguredProvider(body)) {
      throw new Error('Rynna API returned invalid provider data');
    }
    return body;
  }

  private async providerRequest(endpoint: string, method: string, body?: SessionTitleRequest | ContextRequest | ProviderInput | MemorySettingsInput | McpSettings | Workflow | WorkflowStart | WorkflowControl): Promise<unknown> {
    const response = await this.fetcher(endpoint, {
      method,
      ...(body
        ? { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }
        : { headers: { accept: 'application/json' } }),
    });
    if (response.status === 204) return undefined;
    let decoded: unknown;
    try {
      decoded = await response.json();
    } catch {
      throw new Error(`Rynna API returned ${response.status}`);
    }
    if (!response.ok) {
      throw new Error(readApiError(decoded) ?? `Rynna API returned ${response.status}`);
    }
    return decoded;
  }

  async listProfiles(): Promise<ProfileCatalog> {
    const response = await this.fetcher(this.profilesEndpoint, {
      method: 'GET',
      headers: { accept: 'application/json' },
    });
    return this.readProfileCatalog(response);
  }

  async createProfile(profile: Profile): Promise<Profile> {
    return this.savedProfile(this.profilesEndpoint, 'POST', profile);
  }

  async updateProfile(name: string, profile: Profile): Promise<Profile> {
    return this.savedProfile(`${this.profilesEndpoint}/${encodeURIComponent(name)}`, 'PUT', profile);
  }

  async deleteProfile(name: string): Promise<void> {
    const response = await this.fetcher(`${this.profilesEndpoint}/${encodeURIComponent(name)}`, {
      method: 'DELETE',
    });
    if (!response.ok) throw new Error(`Rynna API returned ${response.status}`);
  }

  private async savedProfile(endpoint: string, method: 'POST' | 'PUT', profile: Profile): Promise<Profile> {
    const response = await this.fetcher(endpoint, {
      method,
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(profile),
    });
    const body = await this.readJson(response);
    if (!isProfile(body)) {
      throw new Error('Rynna API returned invalid profile data');
    }
    return body;
  }

  private async readProfileCatalog(response: Response): Promise<ProfileCatalog> {
    const body = await this.readJson(response);
    if (!isProfileCatalog(body)) {
      throw new Error('Rynna API returned invalid profile data');
    }
    return body;
  }

  private async readJson(response: Response): Promise<unknown> {
    let body: unknown;
    try {
      if (response.status === 204) return null;
      body = await response.json();
    } catch {
      throw new Error(
        response.ok ? 'Rynna API returned invalid profile data' : `Rynna API returned ${response.status}`,
      );
    }
    if (!response.ok) {
      throw new Error(readApiError(body) ?? `Rynna API returned ${response.status}`);
    }
    return body;
  }

  async respond(
    request: RespondRequest,
    onDelta?: CompletionDeltaHandler,
    signal?: AbortSignal,
  ): Promise<RespondResponse> {
    if (onDelta || signal) {
      return this.respondStream(request, onDelta ?? (() => {}), signal);
    }
    const response = await this.fetcher(this.endpoint, {
      signal,
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    let body: unknown;
    try {
      body = await response.json();
    } catch {
      throw new Error(
        response.ok ? 'Rynna API returned an invalid response' : `Rynna API returned ${response.status}`,
      );
    }

    if (!response.ok) {
      throw new Error(readApiError(body) ?? `Rynna API returned ${response.status}`);
    }
    if (!isRespondResponse(body)) {
      throw new Error('Rynna API returned an invalid response');
    }

    return body;
  }

  private async respondStream(
    request: RespondRequest,
    onDelta: CompletionDeltaHandler,
    signal?: AbortSignal,
  ): Promise<RespondResponse> {
    const response = await this.fetcher(`${this.endpoint}/stream`, {
      signal,
      method: 'POST',
      headers: {
        accept: 'text/event-stream',
        'content-type': 'application/json',
      },
      body: JSON.stringify(request),
    });
    if (!response.ok) {
      let body: unknown;
      try {
        body = await response.json();
      } catch {
        throw new Error(`Rynna API returned ${response.status}`);
      }
      throw new Error(readApiError(body) ?? `Rynna API returned ${response.status}`);
    }
    if (!response.body) {
      throw new Error('Rynna API returned an invalid stream');
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let pending = '';
    let result: RespondResponse | null = null;

    const processRecords = (complete: boolean) => {
      pending = pending.replaceAll('\r\n', '\n');
      const records = pending.split('\n\n');
      pending = complete ? '' : (records.pop() ?? '');
      for (const record of records) {
        const data = record
          .split('\n')
          .filter((line) => line.startsWith('data:'))
          .map((line) => line.slice(5).trimStart())
          .join('\n');
        if (!data) {
          continue;
        }
        let event: unknown;
        try {
          event = JSON.parse(data);
        } catch {
          throw new Error('Rynna API returned an invalid stream event');
        }
        if (isCompletionDelta(event)) {
          onDelta(event);
        } else if (isDoneEvent(event)) {
          result = { message: event.message };
        } else if (isErrorEvent(event)) {
          // Carry the machine-readable code so callers can branch on the failure
          // rather than matching on prose.
          throw Object.assign(new Error(event.message), { code: event.code });
        } else {
          throw new Error('Rynna API returned an invalid stream event');
        }
      }
    };

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) {
          pending += decoder.decode();
          if (pending) {
            pending += '\n\n';
          }
          processRecords(true);
          break;
        }
        pending += decoder.decode(value, { stream: true });
        processRecords(false);
      }

      if (!result) {
        throw new Error('Rynna API stream ended without a response');
      }
      return result;
    } finally {
      await reader.cancel().catch(() => {});
      reader.releaseLock();
    }
  }
}

function defaultEndpoint(): string {
  const baseUrl = import.meta.env.VITE_RYNNA_API_URL?.replace(/\/$/, '') ?? '';
  return `${baseUrl}/v1/respond`;
}

function isOpenAiAccount(value: unknown): value is OpenAiAccount {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'connected' in value &&
      typeof value.connected === 'boolean' &&
      'method' in value &&
      (value.method === null || value.method === 'chatgpt' || value.method === 'api_key') &&
      (!('plan' in value) || value.plan === undefined || typeof value.plan === 'string'),
  );
}

function isRespondResponse(value: unknown): value is RespondResponse {
  if (!value || typeof value !== 'object' || !('message' in value)) {
    return false;
  }
  const message = value.message;
  return Boolean(
    message &&
      typeof message === 'object' &&
      'role' in message &&
      (message.role === 'assistant' || message.role === 'user') &&
      'content' in message &&
      typeof message.content === 'string',
  );
}

function isCompletionDelta(value: unknown): value is CompletionDelta {
  if (value && typeof value === 'object' && 'kind' in value) {
    if (value.kind === 'tool_finished') return 'id' in value && typeof value.id === 'string';
    if (value.kind === 'tool_started') {
      const call = 'call' in value ? value.call : null;
      return Boolean(call && typeof call === 'object' &&
        'id' in call && typeof call.id === 'string' &&
        'name' in call && typeof call.name === 'string' &&
        'arguments' in call);
    }
  }
  return Boolean(
    value &&
      typeof value === 'object' &&
      'kind' in value &&
      (value.kind === 'thinking' || value.kind === 'content') &&
      'content' in value &&
      typeof value.content === 'string',
  );
}

function isDoneEvent(value: unknown): value is { kind: 'done'; message: RespondResponse['message'] } {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'kind' in value &&
      value.kind === 'done' &&
      'message' in value &&
      isMessage(value.message),
  );
}

function isErrorEvent(
  value: unknown,
): value is { kind: 'error'; message: string; code?: string } {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'kind' in value &&
      value.kind === 'error' &&
      'message' in value &&
      typeof value.message === 'string',
  );
}

function isMessage(value: unknown): value is RespondResponse['message'] {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'role' in value &&
      (value.role === 'assistant' || value.role === 'user') &&
      'content' in value &&
      typeof value.content === 'string',
  );
}

function isProfileCatalog(value: unknown): value is ProfileCatalog {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'default_profile' in value &&
      typeof value.default_profile === 'string' &&
      'provider_ids' in value &&
      Array.isArray(value.provider_ids) &&
      value.provider_ids.every((provider) => typeof provider === 'string') &&
      'profiles' in value &&
      Array.isArray(value.profiles) &&
      value.profiles.every(isProfile) &&
      'configured_profiles' in value &&
      Array.isArray(value.configured_profiles) &&
      value.configured_profiles.every(isProfile),
  );
}

function isProfile(value: unknown): value is Profile {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'name' in value &&
      typeof value.name === 'string' &&
      'providers' in value &&
      Array.isArray(value.providers) &&
      value.providers.length > 0 &&
      value.providers.every(
        (provider) =>
          provider &&
          typeof provider === 'object' &&
          'provider' in provider &&
          typeof provider.provider === 'string' &&
          'model' in provider &&
          typeof provider.model === 'string',
      ) &&
      'active_skills' in value &&
      Array.isArray(value.active_skills) &&
      value.active_skills.every((skill) => typeof skill === 'string') &&
      'mcp_servers' in value &&
      Array.isArray(value.mcp_servers) &&
      value.mcp_servers.every((server) => typeof server === 'string') &&
      'capabilities' in value &&
      Array.isArray(value.capabilities) &&
      value.capabilities.every((capability) => typeof capability === 'string') &&
      'default_project_directory' in value &&
      typeof value.default_project_directory === 'string' &&
      'projects' in value &&
      Array.isArray(value.projects) &&
      value.projects.every(isProject) &&
      'subagents' in value && Array.isArray(value.subagents) &&
      value.subagents.every(isSubagent),
  );
}

function isSubagent(value: unknown): boolean {
  return Boolean(value && typeof value === 'object' &&
    'name' in value && typeof value.name === 'string' &&
    'description' in value && typeof value.description === 'string' &&
    'instructions' in value && typeof value.instructions === 'string');
}

function isProject(value: unknown): boolean {
  return Boolean(
    value && typeof value === 'object' &&
    'name' in value && typeof value.name === 'string' &&
    'directories' in value && Array.isArray(value.directories) &&
    value.directories.every((directory) => typeof directory === 'string') &&
    'default_directory' in value && typeof value.default_directory === 'string',
  );
}

function isConfiguredProvider(value: unknown): value is ConfiguredProvider {
  if (!value || typeof value !== 'object' || !('kind' in value)) return false;
  if (value.kind === 'ollama' || value.kind === 'mlx') {
    return 'api_base' in value && typeof value.api_base === 'string';
  }
  if (value.kind === 'anthropic') {
    return (
      'authentication' in value &&
      (value.authentication === 'api_key' || value.authentication === 'subscription')
    );
  }
  if (value.kind === 'openrouter') return true;
  return (
    value.kind === 'openai' &&
    'authentication' in value &&
    (value.authentication === 'api_key' || value.authentication === 'chatgpt')
  );
}

function readApiError(value: unknown): string | null {
  if (!value || typeof value !== 'object' || !('error' in value)) {
    return null;
  }
  const error = value.error;
  if (!error || typeof error !== 'object' || !('message' in error)) {
    return null;
  }
  return typeof error.message === 'string' ? error.message : null;
}
