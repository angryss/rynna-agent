export type MessageRole = 'user' | 'assistant';

export interface Message {
  role: MessageRole;
  content: string;
  provider_context?: { provider: string; state: unknown };
}

export interface ModelSelection {
  provider: string;
  model: string;
  thinking: 'default' | 'low' | 'medium' | 'high';
}

export interface RespondRequest {
  selection?: ModelSelection;
  session_id?: string;
  profile?: string;
  project?: string;
  prompt: string;
  history: Message[];
}

export interface ContextRequest {
  profile?: string;
  project?: string;
  selection?: ModelSelection;
  session_id?: string;
  history: Message[];
  prompt?: string;
  compact?: boolean;
}
export interface ContextResponse {
  history: Message[];
  size: { current_tokens: number; max_tokens: number };
  compacted: boolean;
  limit_known: boolean;
}
export function isContextResponse(value: unknown): value is ContextResponse {
  if (!value || typeof value !== 'object') return false;
  const v = value as ContextResponse;
  return Array.isArray(v.history) && v.history.every(m => m && (m.role === 'user' || m.role === 'assistant') && typeof m.content === 'string') &&
    !!v.size && Number.isSafeInteger(v.size.current_tokens) && v.size.current_tokens >= 0 && Number.isSafeInteger(v.size.max_tokens) && v.size.max_tokens > 0 &&
    typeof v.compacted === 'boolean' && typeof v.limit_known === 'boolean';
}

export interface RespondResponse {
  message: Message;
}

export type CompletionDelta =
  | { kind: 'thinking'; content: string }
  | { kind: 'content'; content: string }
  | { kind: 'tool_started'; call: { id: string; name: string; arguments: unknown } }
  | { kind: 'tool_finished'; id: string };

export type CompletionDeltaHandler = (delta: CompletionDelta) => void;

export interface ProfileProvider {
  provider: string;
  model: string;
  context_window?: number;
  enabled?: boolean;
  default?: boolean;
}

export interface Profile {
  name: string;
  providers: ProfileProvider[];
  active_skills: string[];
  mcp_servers: string[];
  capabilities: string[];
  default_project_directory: string;
  projects: Project[];
  subagents: Subagent[];
}

export interface Subagent {
  name: string;
  description: string;
  instructions: string;
}

export interface Project {
  name: string;
  directories: string[];
  default_directory: string;
}

export interface ProfileCatalog {
  default_profile: string;
  provider_ids: string[];
  profiles: Profile[];
  configured_profiles: Profile[];
}

export type OpenAiAccount =
  | { connected: false; method: null }
  | { connected: true; method: 'api_key' }
  | { connected: true; method: 'chatgpt'; plan?: string };

export type ConnectOpenAiRequest =
  | { method: 'chatgpt' }
  | { method: 'api_key'; api_key: string };

export type ConfiguredProvider =
  | { kind: 'ollama'; api_base: string }
  | { kind: 'mlx'; api_base: string }
  | { kind: 'openrouter' }
  | { kind: 'openai'; authentication: 'api_key' | 'chatgpt'; reuse_existing?: boolean }
  | { kind: 'anthropic'; authentication: 'api_key' | 'subscription' };

export type ProviderInput =
  | { kind: 'ollama'; api_base: string }
  | { kind: 'mlx'; api_base: string }
  | { kind: 'openrouter' }
  | { kind: 'openai'; authentication: 'chatgpt'; reuse_existing?: boolean }
  | { kind: 'openai'; authentication: 'api_key'; api_key: string }
  | { kind: 'anthropic'; authentication: 'api_key' | 'subscription' };

export type HindsightDeployment = 'cloud' | 'self_hosted';
export type MemorySettings =
  | { kind: 'none' }
  | { kind: 'hindsight'; deployment: HindsightDeployment; api_base: string; bank_id: string; api_key_configured: boolean };
export type MemorySettingsInput =
  | { kind: 'none' }
  | { kind: 'hindsight'; deployment: HindsightDeployment; api_base: string; bank_id: string; api_key?: string };

export function isMemorySettings(value: unknown): value is MemorySettings {
  if (!value || typeof value !== 'object' || !('kind' in value)) return false;
  if (value.kind === 'none') return true;
  return value.kind === 'hindsight' && 'deployment' in value &&
    (value.deployment === 'cloud' || value.deployment === 'self_hosted') &&
    'api_base' in value && typeof value.api_base === 'string' &&
    'bank_id' in value && typeof value.bank_id === 'string' &&
    'api_key_configured' in value && typeof value.api_key_configured === 'boolean';
}

export type McpServer = { enabled?: boolean } & (
  | { transport: 'stdio'; command: string; args?: string[]; env?: Record<string, string> }
  | { transport: 'streamable_http'; url: string; bearer_token_env?: string }
);
export interface McpSettings { mcpServers: Record<string, McpServer> }

export function isMcpSettings(value: unknown): value is McpSettings {
  const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
  if (!object(value) || Object.keys(value).some(k => k !== 'mcpServers') || !object(value.mcpServers)) return false;
  return Object.values(value.mcpServers).every(server => {
    if (!object(server) || (server.enabled !== undefined && typeof server.enabled !== 'boolean')) return false;
    if (server.transport === 'stdio') return Object.keys(server).every(k => ['transport', 'enabled', 'command', 'args', 'env'].includes(k)) &&
      typeof server.command === 'string' && (server.args === undefined || Array.isArray(server.args) && server.args.every(v => typeof v === 'string')) &&
      (server.env === undefined || object(server.env) && Object.values(server.env).every(v => typeof v === 'string'));
    return server.transport === 'streamable_http' && Object.keys(server).every(k => ['transport', 'enabled', 'url', 'bearer_token_env'].includes(k)) &&
      typeof server.url === 'string' && (server.bearer_token_env === undefined || typeof server.bearer_token_env === 'string');
  });
}

export interface SessionTitleRequest { profile?: string; selection?: ModelSelection; prompt: string }

export interface AgentClient {
  sessionTitle?(request: SessionTitleRequest): Promise<string>;
  conversationContext?(request: ContextRequest): Promise<ContextResponse>;
  listWorkflows?(profile: string): Promise<WorkflowMetadata[]>;
  readWorkflow?(profile: string, id: string): Promise<Workflow>;
  saveWorkflow?(profile: string, workflow: Workflow): Promise<Workflow>;
  deleteWorkflow?(profile: string, id: string): Promise<void>;
  startWorkflow?(request: WorkflowStart): Promise<WorkflowRun>;
  readWorkflowRun?(id: string, profile: string, session: string): Promise<WorkflowRun>;
  listWorkflowRuns?(profile: string, session: string): Promise<WorkflowRun[]>;
  controlWorkflow?(id: string, request: WorkflowControl): Promise<WorkflowRun>;
  getMcpSettings?(profile: string): Promise<McpSettings>;
  saveMcpSettings?(settings: McpSettings, profile: string): Promise<McpSettings>;
  getMemorySettings?(profile: string): Promise<MemorySettings>;
  saveMemorySettings?(settings: MemorySettingsInput, profile: string): Promise<MemorySettings>;
  respond(request: RespondRequest, onDelta?: CompletionDeltaHandler, signal?: AbortSignal): Promise<RespondResponse>;
  listProfiles?(): Promise<ProfileCatalog>;
  createProfile?(profile: Profile): Promise<Profile>;
  updateProfile?(name: string, profile: Profile): Promise<Profile>;
  deleteProfile?(name: string): Promise<void>;
  getOpenAiAccount?(): Promise<OpenAiAccount>;
  getExistingOpenAiAccount?(): Promise<OpenAiAccount>;
  connectOpenAi?(request: ConnectOpenAiRequest): Promise<OpenAiAccount>;
  listProviderModels?(profile: string, provider: string): Promise<ProviderModel[]>;
  listProviders?(profile: string): Promise<ConfiguredProvider[]>;
  createProvider?(provider: ProviderInput, profile: string): Promise<ConfiguredProvider>;
  updateProvider?(provider: ProviderInput, profile: string): Promise<ConfiguredProvider>;
  deleteProvider?(kind: ConfiguredProvider['kind'], profile: string): Promise<void>;
}

export interface WorkflowStep { id: string; role: 'work' | 'verify'; executor: 'instructions' | 'subagent'; instructions: string; helper?: string; repeat_target?: string }
export interface Workflow { id: string; name: string; description: string; revision: number; steps: WorkflowStep[] }
export interface WorkflowMetadata { id: string; name: string; description: string; revision: number; read_only: boolean }
export interface WorkflowLimits { steps: number; tool_calls: number; active_seconds: number }
export interface WorkflowCriterion { id: string; text: string }
export interface WorkflowStart { request_id: string; session_id: string; profile: string; project: string | null; selection: ModelSelection; workflow_id: string; goal: string; criteria: WorkflowCriterion[]; limits: WorkflowLimits; initial_context: string }
export type WorkflowStatus = 'running' | 'pausing' | 'cancelling' | 'paused' | 'blocked' | 'completed' | 'failed' | 'cancelled' | 'budget_exhausted';
export interface WorkflowRun {
  id: string; start: WorkflowStart; workflow: Workflow; cursor: number; status: WorkflowStatus; reason: string | null; revision: number; consumed: WorkflowLimits; uncertain: boolean;
  events: { id: number; step_id: string; content: string }[];
  verification: { summary: string; results: { criterion_id: string; verdict: 'met' | 'unmet' | 'unknown'; kind: 'test' | 'artifact' | 'qualitative'; reference: string; excerpt: string }[] } | null;
}
export type WorkflowAction = { action: 'pause' | 'cancel' } | { action: 'resume'; acknowledge_uncertain: boolean } | { action: 'steer'; text: string; criteria: WorkflowCriterion[] | null };
export type WorkflowControl = WorkflowAction & { profile: string; session_id: string; expected_revision: number };

export interface ProviderModel { id: string; context_window?: number }

export function parseProviderModels(value: unknown): ProviderModel[] {
  if (!Array.isArray(value)) throw new Error('Rynna returned invalid model data');
  return value.map((entry) => {
    // Older hosts return only model IDs.
    const model = typeof entry === 'string' ? { id: entry } : entry;
    if (!model || typeof model.id !== 'string' || !model.id.trim()
      || (model.context_window !== undefined && (!Number.isInteger(model.context_window)
        || model.context_window < 1024 || model.context_window > 100_000_000))) {
      throw new Error('Rynna returned invalid model data');
    }
    return model as ProviderModel;
  });
}
