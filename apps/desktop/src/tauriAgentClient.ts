import { Channel, invoke } from '@tauri-apps/api/core';
import { isMcpSettings, isMemorySettings } from '@rynna/ui';
import type {
  AgentClient,
  McpSettings,
  MemorySettings,
  MemorySettingsInput,
  CompletionDelta,
  CompletionDeltaHandler,
  ConnectOpenAiRequest,
  ConfiguredProvider,
  OpenAiAccount,
  Profile,
  ProfileCatalog,
  ProviderInput,
  RespondRequest,
  RespondResponse,
} from '@rynna/ui';

type Invoker = (command: string, args: Record<string, unknown>) => Promise<unknown>;
interface DeltaChannel {
  onmessage: ((message: CompletionDelta) => void) | null;
}
type ChannelFactory = () => DeltaChannel;

export class TauriAgentClient implements AgentClient {
  private readonly invoke: Invoker;
  private readonly createChannel: ChannelFactory;

  constructor(
    invoker: Invoker = (command, args) => invoke(command, args),
    createChannel: ChannelFactory = () => new Channel<CompletionDelta>(),
  ) {
    this.invoke = invoker;
    this.createChannel = createChannel;
  }

  async getMcpSettings(profile: string): Promise<McpSettings> {
    const settings = await this.invoke('get_mcp_settings', { profile });
    if (!isMcpSettings(settings)) throw new Error('Rynna desktop returned invalid MCP settings');
    return settings;
  }

  async saveMcpSettings(settings: McpSettings, profile: string): Promise<McpSettings> {
    const saved = await this.invoke('save_mcp_settings', { settings, profile });
    if (!isMcpSettings(saved)) throw new Error('Rynna desktop returned invalid MCP settings');
    return saved;
  }

  async getMemorySettings(profile: string): Promise<MemorySettings> {
    const settings = await this.invoke('get_memory_settings', { profile });
    if (!isMemorySettings(settings)) throw new Error('Rynna desktop returned invalid memory settings');
    return settings;
  }

  async saveMemorySettings(settings: MemorySettingsInput, profile: string): Promise<MemorySettings> {
    const saved = await this.invoke('save_memory_settings', { settings, profile });
    if (!isMemorySettings(saved)) throw new Error('Rynna desktop returned invalid memory settings');
    return saved;
  }

  async listProfiles(): Promise<ProfileCatalog> {
    const profiles = await this.invoke('profiles', {});
    if (!isProfileCatalog(profiles)) {
      throw new Error('Rynna desktop returned invalid profile data');
    }
    return profiles;
  }

  async createProfile(profile: Profile): Promise<Profile> {
    const saved = await this.invoke('create_profile', { profile });
    if (!isProfile(saved)) {
      throw new Error('Rynna desktop returned invalid profile data');
    }
    return saved;
  }

  async updateProfile(name: string, profile: Profile): Promise<Profile> {
    const saved = await this.invoke('update_profile', { name, profile });
    if (!isProfile(saved)) {
      throw new Error('Rynna desktop returned invalid profile data');
    }
    return saved;
  }

  async deleteProfile(name: string): Promise<void> {
    await this.invoke('delete_profile', { name });
  }

  async getOpenAiAccount(): Promise<OpenAiAccount> {
    const account = await this.invoke('openai_account', {});
    if (!isOpenAiAccount(account)) {
      throw new Error('Rynna desktop returned invalid OpenAI account data');
    }
    return account;
  }

  async getExistingOpenAiAccount(): Promise<OpenAiAccount> {
    const account = await this.invoke('existing_openai_account', {});
    if (!isOpenAiAccount(account)) {
      throw new Error('Rynna desktop returned invalid existing OpenAI account data');
    }
    return account;
  }

  async connectOpenAi(request: ConnectOpenAiRequest): Promise<OpenAiAccount> {
    const account = await this.invoke('connect_openai', { request });
    if (!isOpenAiAccount(account)) {
      throw new Error('Rynna desktop returned invalid OpenAI account data');
    }
    return account;
  }

  async listProviders(profile: string): Promise<ConfiguredProvider[]> {
    const providers = await this.invoke('list_providers', { profile });
    if (!Array.isArray(providers) || !providers.every(isConfiguredProvider)) {
      throw new Error('Rynna desktop returned invalid provider data');
    }
    return providers;
  }

  async createProvider(provider: ProviderInput, profile: string): Promise<ConfiguredProvider> {
    return this.saveProvider('create_provider', provider, profile);
  }

  async updateProvider(provider: ProviderInput, profile: string): Promise<ConfiguredProvider> {
    return this.saveProvider('update_provider', provider, profile);
  }

  async deleteProvider(kind: ConfiguredProvider['kind'], profile: string): Promise<void> {
    await this.invoke('delete_provider', { kind, profile });
  }

  private async saveProvider(
    command: string,
    provider: ProviderInput,
    profile: string,
  ): Promise<ConfiguredProvider> {
    const saved = await this.invoke(command, { provider, profile });
    if (!isConfiguredProvider(saved)) {
      throw new Error('Rynna desktop returned invalid provider data');
    }
    return saved;
  }

  async respond(
    request: RespondRequest,
    onDelta?: CompletionDeltaHandler,
  ): Promise<RespondResponse> {
    let command = 'respond';
    let args: Record<string, unknown> = { request };
    let invalidDelta = false;
    if (onDelta) {
      command = 'respond_stream';
      const onEvent = this.createChannel();
      onEvent.onmessage = (message) => {
        if (isCompletionDelta(message)) {
          onDelta(message);
        } else {
          invalidDelta = true;
        }
      };
      args = { request, onEvent };
    }

    const response = await this.invoke(command, args);
    if (invalidDelta) {
      throw new Error('Rynna desktop returned invalid stream data');
    }
    if (!isRespondResponse(response)) {
      throw new Error('Rynna desktop returned an invalid response');
    }
    return response;
  }
}

function isCompletionDelta(value: unknown): value is CompletionDelta {
  return Boolean(
    value &&
      typeof value === 'object' &&
      'kind' in value &&
      (value.kind === 'thinking' || value.kind === 'content') &&
      'content' in value &&
      typeof value.content === 'string',
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
      (!('default_workspace_directory' in value) || typeof value.default_workspace_directory === 'string') &&
      (!('workspaces' in value) || Array.isArray(value.workspaces) && value.workspaces.every(isWorkspace)),
  );
}

function isWorkspace(value: unknown): boolean {
  return Boolean(
    value && typeof value === 'object' &&
    'name' in value && typeof value.name === 'string' &&
    'directories' in value && Array.isArray(value.directories) &&
    value.directories.every((directory) => typeof directory === 'string') &&
    'default_directory' in value && typeof value.default_directory === 'string',
  );
}

function isOpenAiAccount(value: unknown): value is OpenAiAccount {
  if (!value || typeof value !== 'object' || !('connected' in value)) {
    return false;
  }
  if (value.connected === false) {
    return 'method' in value && value.method === null;
  }
  if (value.connected !== true || !('method' in value)) {
    return false;
  }
  if (value.method === 'api_key') {
    return true;
  }
  return (
    value.method === 'chatgpt' &&
    (!('plan' in value) || value.plan === undefined || typeof value.plan === 'string')
  );
}

function isConfiguredProvider(value: unknown): value is ConfiguredProvider {
  if (!value || typeof value !== 'object' || !('kind' in value)) return false;
  if (value.kind === 'ollama') {
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
