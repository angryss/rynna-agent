import { newSessionId } from './sessions';
import { WorkflowSettings } from './components/workflow-settings';
import { WorkflowPanel, workflowTerminal } from './components/workflow-panel';
import type { WorkflowRun } from './contracts';
import { ModelSelector } from './components/model-selector';
import { McpSettingsPanel } from './components/mcp-settings';
import { FormEvent, useEffect, useRef, useState } from 'react';

import { MemorySettingsPanel } from './components/memory-settings';
import { ThemeToggle } from './components/theme-toggle';
import { Typeahead } from './components/typeahead';
import { SubagentSettings } from './components/subagent-settings';
import { ProjectSettings } from './components/project-settings';
import { SessionSidebar } from './components/session-sidebar';
import { Badge } from './components/ui/badge';
import { Button } from './components/ui/button';
import { Input } from './components/ui/input';
import { Textarea } from './components/ui/textarea';
import type {
  AgentClient,
  CompletionDelta,
  ConfiguredProvider,
  Message,
  ModelSelection,
  OpenAiAccount,
  Profile,
  ProfileProvider,
  ProviderInput,
} from './contracts';
import {
  deleteSession,
  isSessionDeleted,
  mergeSessions,
  readSessions,
  reconcileProjectSessions,
  sessionName,
  sessionsFromStorageEvent,
  writeSessions,
  type Session,
} from './sessions';

export interface AppProps {
  client: AgentClient;
}

interface ThinkingMessage {
  role: 'thinking';
  content: string;
  expanded: boolean;
}

type DisplayMessage = Message | ThinkingMessage;

const PROVIDER_KINDS: readonly ConfiguredProvider['kind'][] = [
  'anthropic',
  'mlx',
  'ollama',
  'openai',
  'openrouter',
];

function sortedProfiles(profiles: Profile[]): Profile[] {
  return [...profiles].sort((left, right) => left.name.localeCompare(right.name));
}

function conversationHistory(messages: DisplayMessage[]): Message[] {
  return messages.filter((message): message is Message => message.role !== 'thinking');
}

function appendDelta(messages: DisplayMessage[], delta: CompletionDelta): DisplayMessage[] {
  if (!delta.content) {
    return messages;
  }
  if (delta.kind === 'thinking') {
    const last = messages.at(-1);
    if (last?.role === 'thinking') {
      return [
        ...messages.slice(0, -1),
        { ...last, content: last.content + delta.content, expanded: true },
      ];
    }
    return [...messages, { role: 'thinking', content: delta.content, expanded: true }];
  }

  const collapsed = messages.map((message) =>
    message.role === 'thinking' ? { ...message, expanded: false } : message,
  );
  const last = collapsed.at(-1);
  if (last?.role === 'assistant') {
    return [
      ...collapsed.slice(0, -1),
      { ...last, content: last.content + delta.content },
    ];
  }
  return [...collapsed, { role: 'assistant', content: delta.content }];
}

function finalizeResponse(messages: DisplayMessage[], message: Message): DisplayMessage[] {
  const collapsed = messages.map((candidate) =>
    candidate.role === 'thinking' ? { ...candidate, expanded: false } : candidate,
  );
  if (!message.content) {
    return collapsed;
  }

  const last = collapsed.at(-1);
  if (last?.role === 'assistant') {
    return [...collapsed.slice(0, -1), message];
  }
  return [...collapsed, message];
}

export function App({ client }: AppProps) {
  const sessionId = useRef<string | null>(null);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [deletingSession, setDeletingSession] = useState(false);
  const [sessions, setSessions] = useState<Session[]>(readSessions);
  const [messages, setMessages] = useState<DisplayMessage[]>([]);
  const workflowDraftId = useRef(newSessionId());
  const workflowEvents = useRef(new Set<string>());
  const [workflowRunning, setWorkflowRunning] = useState(false);
  const [input, setInput] = useState('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [configuredProfiles, setConfiguredProfiles] = useState<Profile[]>([]);
  const [chatSelection, setChatSelection] = useState<{ profile: string; value?: ModelSelection }>();
  const [chatProject, setChatProject] = useState<{ profile: string; name?: string }>();
  const [selectedProfile, setSelectedProfile] = useState<string | null>(null);
  const [selectedSettingsProfile, setSelectedSettingsProfile] = useState<string | null>(null);
  const [openAiAccount, setOpenAiAccount] = useState<OpenAiAccount | null>(null);
  const [existingOpenAiAccount, setExistingOpenAiAccount] = useState<OpenAiAccount | null>(null);
  const [discoveringExistingOpenAiAccount, setDiscoveringExistingOpenAiAccount] = useState(
    Boolean(client.getExistingOpenAiAccount),
  );
  const [showOpenAi, setShowOpenAi] = useState(false);
  const [apiKey, setApiKey] = useState('');
  const [connectingOpenAi, setConnectingOpenAi] = useState(false);
  const [view, setView] = useState<'chat' | 'settings'>('chat');
  const [providerSettings, setProviderSettings] = useState<ConfiguredProvider[]>([]);
  const [editingProvider, setEditingProvider] = useState<ConfiguredProvider['kind'] | null>(null);
  const [providerKind, setProviderKind] = useState<ConfiguredProvider['kind']>('ollama');
  const [providerTypeQuery, setProviderTypeQuery] = useState('Ollama');
  const [providerTypeOpen, setProviderTypeOpen] = useState(false);
  const [providerTypeDirty, setProviderTypeDirty] = useState(false);
  const [activeProviderKind, setActiveProviderKind] = useState<ConfiguredProvider['kind'] | null>('ollama');
  const [ollamaApiBase, setOllamaApiBase] = useState('http://127.0.0.1:11434/v1');
  const [openAiAuthentication, setOpenAiAuthentication] = useState<'api_key' | 'chatgpt'>('chatgpt');
  const [anthropicAuthentication, setAnthropicAuthentication] = useState<'api_key' | 'subscription'>('subscription');
  const [reuseExistingChatgpt, setReuseExistingChatgpt] = useState<boolean | null>(null);
  const [providerApiKey, setProviderApiKey] = useState('');
  const [savingProvider, setSavingProvider] = useState(false);
  const [settingsSection, setSettingsSection] = useState<'workflows' | 'profiles' | 'projects' | 'subagents' | 'provider-credentials' | 'models' | 'memory' | 'mcp'>(
    client.createProfile || client.updateProfile || client.deleteProfile
      ? 'profiles'
      : client.listProviders ? 'provider-credentials' : client.getMemorySettings ? 'memory' : 'mcp',
  );
  const [addingProfile, setAddingProfile] = useState(false);
  const [savingProfile, setSavingProfile] = useState(false);
  const [profileName, setProfileName] = useState('');
  const [profileSkills, setProfileSkills] = useState('');
  const [profileProviders, setProfileProviders] = useState<ProfileProvider[]>([]);
  const [catalogProviderIds, setCatalogProviderIds] = useState<string[]>([]);
  const [modelProvider, setModelProvider] = useState('');
  const [customModel, setCustomModel] = useState('');
  const [mlxApiBase, setMlxApiBase] = useState('http://127.0.0.1:8000/v1');
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const openAiAccountRequest = useRef(0);
  const providerMutationRevision = useRef(0);

  useEffect(() => {
    const merged = writeSessions(sessions);
    if (merged.length !== sessions.length || merged.some((session, index) => session.id !== sessions[index]?.id)) setSessions(merged);
  }, [sessions]);

  useEffect(() => {
    const synchronizeSessions = (event: StorageEvent) => {
      const incoming = sessionsFromStorageEvent(event);
      if (incoming) {
        setSessions(current => mergeSessions(current, incoming).filter(session => !isSessionDeleted(session.id)));
        if (sessionId.current && isSessionDeleted(sessionId.current)) resetConversation();
      }
    };
    window.addEventListener('storage', synchronizeSessions);
    return () => window.removeEventListener('storage', synchronizeSessions);
  }, []);

  useEffect(() => {
    let active = true;
    if (!client.listProfiles) {
      return () => {
        active = false;
      };
    }

    void client
      .listProfiles()
      .then((catalog) => {
        if (!active) {
          return;
        }
        setProfiles(catalog.profiles);
        setConfiguredProfiles(catalog.configured_profiles ?? catalog.profiles);
        setCatalogProviderIds(catalog.provider_ids);
        setSelectedProfile(catalog.default_profile);
        setSelectedSettingsProfile(catalog.default_profile);
      })
      .catch((profileError: unknown) => {
        if (active) {
          setError(
            profileError instanceof Error
              ? profileError.message
              : 'Rynna could not load profiles',
          );
        }
      });

    return () => {
      active = false;
    };
  }, [client]);

  useEffect(() => {
    let active = true;
    const request = ++openAiAccountRequest.current;
    if (client.getOpenAiAccount) {
      void client
        .getOpenAiAccount()
        .then((account) => {
          if (active && request === openAiAccountRequest.current) setOpenAiAccount(account);
        })
        .catch(() => {
          if (active && request === openAiAccountRequest.current) {
            setOpenAiAccount({ connected: false, method: null });
          }
        });
    }
    return () => {
      active = false;
    };
  }, [client]);

  useEffect(() => {
    let active = true;
    if (client.getExistingOpenAiAccount) {
      setDiscoveringExistingOpenAiAccount(true);
      void client
        .getExistingOpenAiAccount()
        .then((account) => {
          if (active) setExistingOpenAiAccount(account);
        })
        .catch(() => {
          if (active) setExistingOpenAiAccount({ connected: false, method: null });
        })
        .finally(() => {
          if (active) setDiscoveringExistingOpenAiAccount(false);
        });
    } else {
      setDiscoveringExistingOpenAiAccount(false);
    }
    return () => {
      active = false;
    };
  }, [client]);

  useEffect(() => {
    let active = true;
    const revision = providerMutationRevision.current;
    if (client.listProviders && (selectedSettingsProfile || !client.listProfiles)) {
      const providerProfile = selectedSettingsProfile ?? 'default';
      void client
        .listProviders(providerProfile)
        .then((providers) => {
          if (active && revision === providerMutationRevision.current) {
            setProviderSettings(providers);
          }
        })
        .catch((providerError: unknown) => {
          if (active) {
            setError(
              providerError instanceof Error
                ? providerError.message
                : 'Rynna could not load provider settings',
            );
          }
        });
    }
    return () => {
      active = false;
    };
  }, [client, selectedSettingsProfile]);

  const activeProfile = profiles.find((profile) => profile.name === selectedProfile);
  const selection = chatSelection?.profile === selectedProfile && activeProfile?.providers.some(pair =>
    pair.enabled !== false && pair.provider === chatSelection.value?.provider && pair.model === chatSelection.value?.model)
    ? chatSelection.value : undefined;
  const project = chatProject?.profile === selectedProfile && activeProfile?.projects.some(candidate => candidate.name === chatProject.name)
    ? chatProject.name : undefined;
  const activeConfiguredProfile = configuredProfiles.find(
    (profile) => profile.name === selectedSettingsProfile,
  );
  const canEditProfiles = Boolean(client.createProfile || client.updateProfile || client.deleteProfile);
  const canOpenSettings = Boolean(client.listProviders || canEditProfiles || client.getMemorySettings || client.getMcpSettings);
  const selectedModelKeys = new Set(
    selectedModels.map((model) => `${modelProvider}\u0000${model}`),
  );
  const disablingWouldRemoveEveryEnabledModel = Boolean(
    activeConfiguredProfile &&
      activeConfiguredProfile.providers.every(
        (provider) =>
          provider.enabled === false ||
          selectedModelKeys.has(`${provider.provider}\u0000${provider.model}`),
      ),
  );

  useEffect(() => {
    if (addingProfile) {
      return;
    }
    if (!activeConfiguredProfile) {
      setProfileName('');
      setProfileSkills('');
      setProfileProviders([]);
      return;
    }
    setProfileName(activeConfiguredProfile.name);
    setProfileSkills(activeConfiguredProfile.active_skills.join('\n'));
    setProfileProviders(activeConfiguredProfile.providers.map((provider) => ({ ...provider })));
  }, [activeConfiguredProfile, addingProfile]);

  useEffect(() => {
    const providerIds = catalogProviderIds.length > 0
      ? catalogProviderIds
      : activeConfiguredProfile
        ? [...new Set(activeConfiguredProfile.providers.map((provider) => provider.provider))]
        : [];
    if (!providerIds.includes(modelProvider)) {
      setModelProvider(providerIds[0] ?? '');
    }
    setSelectedModels([]);
    setCustomModel('');
  }, [activeConfiguredProfile, catalogProviderIds, modelProvider]);

  function selectProfile(name: string) {
    setChatSelection(undefined);
    setChatProject(undefined);
    setSelectedProfile(name);
    setAddingProfile(false);
    setMessages([]);
    sessionId.current = null;
    workflowDraftId.current = newSessionId();
    setWorkflowRunning(false);
    setActiveSessionId(null);
    setError(null);
  }

  function startNewSession(projectName: string | undefined) {
    setChatProject(selectedProfile ? { profile: selectedProfile, name: projectName } : undefined);
    resetConversation();
  }

  function resetConversation() {
    setMessages([]);
    setInput('');
    sessionId.current = null;
    workflowDraftId.current = newSessionId();
    setWorkflowRunning(false);
    setActiveSessionId(null);
    setError(null);
  }

  async function removeSession(session: Session): Promise<boolean> {
    if (pending || deletingSession) return false;
    if (workflowRunning) {
      setError('Pause or finish the active workflow before deleting a session.');
      return false;
    }
    setDeletingSession(true);
    setError(null);
    try {
      if (client.listWorkflowRuns) {
        const runs = await client.listWorkflowRuns(session.profile, session.id);
        if (runs.some(run => !workflowTerminal(run))) {
          throw new Error('Cancel or finish this session’s workflow before deleting it.');
        }
      }
      const remaining = deleteSession(session.id, sessions);
      setSessions(remaining);
      if (sessionId.current === session.id) resetConversation();
      return true;
    } catch (deleteError) {
      setError(deleteError instanceof Error ? deleteError.message : 'Rynna could not delete the session');
      return false;
    } finally {
      setDeletingSession(false);
    }
  }

  function selectSession(session: Session) {
    setSelectedProfile(session.profile || null);
    setChatProject(session.profile ? { profile: session.profile, name: session.project ?? undefined } : undefined);
    setChatSelection(undefined);
    setMessages(session.messages);
    setInput('');
    sessionId.current = session.id;
    setWorkflowRunning(false);
    setActiveSessionId(session.id);
    setError(null);
  }

  function selectSettingsProfile(name: string) {
    setSelectedSettingsProfile(name);
    setAddingProfile(false);
    setError(null);
  }

  function beginAddProfile() {
    setAddingProfile(true);
    setProfileName('');
    setProfileSkills('');
    setProfileProviders(
      activeConfiguredProfile?.providers.map((provider) => ({ ...provider })) ?? [
        { provider: 'ollama', model: 'qwen3:8b', enabled: true, default: true },
      ],
    );
    setError(null);
  }

  function profileDraft(): Profile {
    const firstEnabledIndex = profileProviders.findIndex((provider) => provider.enabled !== false);
    const hasExplicitDefault = profileProviders.some(
      (provider) => provider.enabled !== false && provider.default === true,
    );
    return {
      name: profileName.trim(),
      providers: profileProviders.map((provider, index) => ({
        provider: provider.provider.trim(),
        model: provider.model.trim(),
        enabled: provider.enabled !== false,
        default:
          provider.enabled !== false &&
          (provider.default === true || (!hasExplicitDefault && index === firstEnabledIndex)),
      })),
      active_skills: profileSkills.split('\n').map((skill) => skill.trim()).filter(Boolean),
      mcp_servers: addingProfile ? [] : (activeConfiguredProfile?.mcp_servers ?? []),
      capabilities: addingProfile ? [] : (activeConfiguredProfile?.capabilities ?? []),
      default_project_directory: addingProfile ? '.' : (activeConfiguredProfile?.default_project_directory ?? '.'),
      projects: addingProfile ? [] : (activeConfiguredProfile?.projects ?? []),
      subagents: addingProfile ? [] : (activeConfiguredProfile?.subagents ?? []),
    };
  }

  async function saveProfile(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (savingProfile) return;
    const draft = profileDraft();
    if (
      !draft.name ||
      draft.providers.length === 0 ||
      draft.providers.some((provider) => !provider.provider || !provider.model)
    ) return;
    const save = addingProfile ? client.createProfile : client.updateProfile;
    if (!save) return;
    setSavingProfile(true);
    setError(null);
    try {
      const saved = addingProfile
        ? await client.createProfile!(draft)
        : await client.updateProfile!(selectedSettingsProfile ?? draft.name, draft);
      setConfiguredProfiles((current) => {
        const withoutPrevious = addingProfile
          ? current
          : current.filter((profile) => profile.name !== selectedSettingsProfile);
        return [...withoutPrevious.filter((profile) => profile.name !== saved.name), saved];
      });
      setSelectedSettingsProfile(saved.name);
      if (!addingProfile && selectedSettingsProfile && selectedSettingsProfile !== saved.name) {
        setSessions(current => current.map(session => session.profile === selectedSettingsProfile
          ? { ...session, profile: saved.name }
          : session));
      }
      setAddingProfile(false);
      setProfileName(saved.name);
      setProfileProviders(saved.providers.map((provider) => ({ ...provider })));
    } catch (profileError) {
      setError(
        profileError instanceof Error ? profileError.message : 'Rynna could not save the profile',
      );
    } finally {
      setSavingProfile(false);
    }
  }

  async function addCustomModel(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const model = customModel.trim();
    if (!activeConfiguredProfile || !modelProvider || !model || savingProfile) return;
    if (activeConfiguredProfile.providers.some((entry) => entry.provider === modelProvider && entry.model === model)) {
      setError('This model is already configured for this provider.');
      return;
    }
    await saveModelSettings([
      ...activeConfiguredProfile.providers,
      { provider: modelProvider, model, enabled: true, default: false },
    ]);
  }

  async function saveModelSettings(nextProviders: ProfileProvider[]) {
    if (!client.updateProfile || !activeConfiguredProfile || savingProfile) return;
    setSavingProfile(true);
    setError(null);
    try {
      const saved = await client.updateProfile(activeConfiguredProfile.name, {
        ...activeConfiguredProfile,
        providers: nextProviders,
      });
      setConfiguredProfiles((current) =>
        current.map((profile) => (profile.name === saved.name ? saved : profile)),
      );
    } catch (profileError) {
      setError(
        profileError instanceof Error ? profileError.message : 'Rynna could not save model settings',
      );
    } finally {
      setSavingProfile(false);
    }
  }

  function setSelectedModelState(enabled: boolean) {
    if (!activeConfiguredProfile || selectedModels.length === 0) return;
    const selected = new Set(selectedModels);
    const nextProviders = activeConfiguredProfile.providers.map((provider) =>
      provider.provider === modelProvider && selected.has(provider.model)
        ? { ...provider, enabled, default: enabled ? provider.default : false }
        : { ...provider },
    );
    const enabledProviders = nextProviders.filter((provider) => provider.enabled !== false);
    if (enabledProviders.length > 0 && !enabledProviders.some((provider) => provider.default)) {
      enabledProviders[0]!.default = true;
    }
    void saveModelSettings(nextProviders);
  }

  function setDefaultModel(model: string) {
    if (!activeConfiguredProfile) return;
    void saveModelSettings(
      activeConfiguredProfile.providers.map((provider) => ({
        ...provider,
        enabled:
          provider.provider === modelProvider && provider.model === model
            ? true
            : provider.enabled,
        default: provider.provider === modelProvider && provider.model === model,
      })),
    );
  }

  async function removeProfile() {
    if (!client.deleteProfile || savingProfile || addingProfile || !selectedSettingsProfile) return;
    if (configuredProfiles.length <= 1) return;
    setSavingProfile(true);
    setError(null);
    try {
      await client.deleteProfile(selectedSettingsProfile);
      const remaining = configuredProfiles.filter(
        (profile) => profile.name !== selectedSettingsProfile,
      );
      setConfiguredProfiles(remaining);
      setProfiles((current) => current.filter((profile) => profile.name !== selectedSettingsProfile));
      const next = remaining[0]?.name ?? null;
      setSelectedSettingsProfile(next);
      if (selectedProfile === selectedSettingsProfile) {
        setSelectedProfile(profiles.find((profile) => profile.name !== selectedSettingsProfile)?.name ?? null);
        setMessages([]);
        sessionId.current = null;
    workflowDraftId.current = newSessionId();
    setWorkflowRunning(false);
        setActiveSessionId(null);
      }
    } catch (profileError) {
      setError(
        profileError instanceof Error ? profileError.message : 'Rynna could not delete the profile',
      );
    } finally {
      setSavingProfile(false);
    }
  }

  async function connectOpenAi(method: 'chatgpt' | 'api_key') {
    if (!client.connectOpenAi || connectingOpenAi) return;
    openAiAccountRequest.current += 1;
    setConnectingOpenAi(true);
    setError(null);
    try {
      const account = await client.connectOpenAi(
        method === 'chatgpt' ? { method } : { method, api_key: apiKey },
      );
      setOpenAiAccount(account);
      setShowOpenAi(false);
    } catch (connectError) {
      setError(
        connectError instanceof Error
          ? connectError.message
          : 'Rynna could not connect OpenAI',
      );
    } finally {
      setApiKey('');
      setConnectingOpenAi(false);
    }
  }

  function matchingProviderKinds(query: string, showAll = !providerTypeDirty) {
    return PROVIDER_KINDS.filter(
      (kind) =>
        !providerSettings.some((provider) => provider.kind === kind) &&
        (showAll || providerTitle(kind).toLowerCase().includes(query.toLowerCase())),
    );
  }

  function beginAddProvider() {
    const availableKind = (['ollama', 'openai', 'openrouter', 'anthropic', 'mlx'] as const).find(
      (kind) => !providerSettings.some((provider) => provider.kind === kind),
    ) ?? 'ollama';
    setEditingProvider(availableKind);
    setProviderKind(availableKind);
    setProviderTypeQuery(providerTitle(availableKind));
    setProviderTypeOpen(false);
    setProviderTypeDirty(false);
    setActiveProviderKind(availableKind);
    setOllamaApiBase('http://127.0.0.1:11434/v1');
    setMlxApiBase('http://127.0.0.1:8000/v1');
    setOpenAiAuthentication('chatgpt');
    setAnthropicAuthentication('subscription');
    setReuseExistingChatgpt(null);
    setProviderApiKey('');
  }

  function beginEditProvider(provider: ConfiguredProvider) {
    setEditingProvider(provider.kind);
    setProviderKind(provider.kind);
    setProviderTypeQuery(providerTitle(provider.kind));
    setProviderTypeOpen(false);
    setProviderTypeDirty(false);
    setActiveProviderKind(provider.kind);
    if (provider.kind === 'ollama') setOllamaApiBase(provider.api_base);
    else if (provider.kind === 'mlx') setMlxApiBase(provider.api_base);
    else if (provider.kind === 'openai') setOpenAiAuthentication(provider.authentication);
    else if (provider.kind === 'anthropic') setAnthropicAuthentication(provider.authentication);
    setReuseExistingChatgpt(null);
    setProviderApiKey('');
  }

  function selectProviderKind(kind: ConfiguredProvider['kind']) {
    setProviderKind(kind);
    setEditingProvider(kind);
    setProviderTypeQuery(providerTitle(kind));
    setProviderTypeOpen(false);
    setProviderTypeDirty(false);
    setActiveProviderKind(kind);
  }

  async function refreshOpenAiAccountStatus() {
    if (!client.getOpenAiAccount) return;
    const request = ++openAiAccountRequest.current;
    try {
      const account = await client.getOpenAiAccount();
      if (request === openAiAccountRequest.current) setOpenAiAccount(account);
    } catch {
      if (request === openAiAccountRequest.current) {
        setOpenAiAccount({ connected: false, method: null });
      }
    }
  }

  async function saveProvider(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (savingProvider) return;
    const existing = providerSettings.some((provider) => provider.kind === editingProvider);
    const input: ProviderInput =
      providerKind === 'ollama'
        ? { kind: 'ollama', api_base: ollamaApiBase.trim() }
        : providerKind === 'mlx'
          ? { kind: 'mlx', api_base: mlxApiBase.trim() }
        : providerKind === 'openrouter'
          ? { kind: 'openrouter' }
        : providerKind === 'anthropic'
          ? { kind: 'anthropic', authentication: anthropicAuthentication }
          : openAiAuthentication === 'chatgpt'
          ? {
              kind: 'openai',
              authentication: 'chatgpt',
              ...(reuseExistingChatgpt === true ? { reuse_existing: true } : {}),
            }
          : { kind: 'openai', authentication: 'api_key', api_key: providerApiKey };
    const save = existing ? client.updateProvider : client.createProvider;
    if (!save) return;
    const providerProfile = selectedSettingsProfile ?? 'default';
    setSavingProvider(true);
    setError(null);
    try {
      const saved = await save.call(client, input, providerProfile);
      providerMutationRevision.current += 1;
      setProviderSettings((current) => [
        ...current.filter((provider) => provider.kind !== saved.kind),
        saved,
      ]);
      if (saved.kind === 'openai') await refreshOpenAiAccountStatus();
      setEditingProvider(null);
    } catch (providerError) {
      setError(
        providerError instanceof Error
          ? providerError.message
          : 'Rynna could not save the provider',
      );
    } finally {
      setProviderApiKey('');
      setSavingProvider(false);
    }
  }

  async function removeProvider(kind: ConfiguredProvider['kind']) {
    if (!client.deleteProvider || savingProvider) return;
    const providerProfile = selectedSettingsProfile ?? 'default';
    setSavingProvider(true);
    setError(null);
    try {
      await client.deleteProvider(kind, providerProfile);
      providerMutationRevision.current += 1;
      setProviderSettings((current) => current.filter((provider) => provider.kind !== kind));
      if (kind === 'openai') await refreshOpenAiAccountStatus();
      setEditingProvider(null);
    } catch (providerError) {
      setError(
        providerError instanceof Error
          ? providerError.message
          : 'Rynna could not delete the provider',
      );
    } finally {
      setSavingProvider(false);
    }
  }

  const workflowSession = activeSessionId ?? workflowDraftId.current;
  const selectedWorkflow = sessions.find(s => s.id === workflowSession)?.workflow_id ?? '';
  function saveWorkflowSelection(id: string) {
    const now = new Date().toISOString();
    sessionId.current = workflowSession;
    setActiveSessionId(workflowSession);
    setSessions(current => {
      const existing = current.find(s => s.id === workflowSession);
      return [{ id: workflowSession, name: 'Workflow conversation', profile: selectedProfile ?? '', project: project ?? null,
        messages: conversationHistory(messages), created_at: now, updated_at: now, ...existing, workflow_id: id }, ...current.filter(s => s.id !== workflowSession)];
    });
  }
  function receiveWorkflow(run: WorkflowRun) {
    if (isSessionDeleted(run.start.session_id)) return;
    setWorkflowRunning(['running', 'pausing', 'cancelling'].includes(run.status));
    const saved = sessions.find(s => s.id === run.start.session_id);
    const known = new Set(saved?.workflow_event_ids ?? []);
    const events = run.events.filter(e => !known.has(`${run.id}:${e.id}`) && !workflowEvents.current.has(`${run.id}:${e.id}`));
    if (!events.length && saved?.workflow_run_id === run.id) return;
    events.forEach(e => workflowEvents.current.add(`${run.id}:${e.id}`));
    const additions: Message[] = events.map(e => ({ role: 'assistant', content: e.content }));
    if (additions.length) setMessages(current => [...current, ...additions]);
    const now = new Date().toISOString();
    setSessions(current => {
      const existing = current.find(s => s.id === run.start.session_id);
      const entry: Session = { id: run.start.session_id, profile: run.start.profile,
        project: run.start.project, created_at: now, ...existing, name: existing?.name && existing.name !== 'Workflow conversation' ? existing.name : sessionName(run.start.goal), updated_at: now, workflow_run_id: run.id,
        messages: [...(existing?.messages ?? []), ...additions], workflow_event_ids: [...(existing?.workflow_event_ids ?? []), ...events.map(e => `${run.id}:${e.id}`)] };
      return [entry, ...current.filter(s => s.id !== entry.id)];
    });
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const prompt = input.trim();
    if (!prompt || pending || workflowRunning || deletingSession) {
      return;
    }

    const displayHistory = messages;
    const history = conversationHistory(displayHistory);
    setError(null);
    setInput('');
    setPending(true);
    setMessages([...displayHistory, { role: 'user', content: prompt }]);

    const currentSessionId = sessionId.current ?? newSessionId();
    sessionId.current = currentSessionId;
    try {
      const response = await client.respond({
        session_id: currentSessionId,
        ...(selection ? { selection } : {}),
        ...(selectedProfile ? { profile: selectedProfile } : {}),
        ...(project ? { project } : {}),
        prompt,
        history,
      }, (delta) => {
        if (sessionId.current === currentSessionId) setMessages((current) => appendDelta(current, delta));
      });
      if (sessionId.current !== currentSessionId || isSessionDeleted(currentSessionId)) return;
      setMessages((current) => finalizeResponse(current, response.message));
      const now = new Date().toISOString();
      const visibleMessages = [...history, { role: 'user' as const, content: prompt }, response.message];
      setSessions((current) => {
        const existing = current.find(session => session.id === currentSessionId);
        const saved: Session = {
          ...existing,
          id: currentSessionId,
          name: existing?.name ?? sessionName(prompt),
          profile: selectedProfile ?? '',
          project: project ?? null,
          messages: visibleMessages,
          created_at: existing?.created_at ?? now,
          updated_at: now,
        };
        return [saved, ...current.filter(session => session.id !== currentSessionId)];
      });
      setActiveSessionId(currentSessionId);
    } catch (requestError) {
      if (sessionId.current !== currentSessionId) return;
      setError(requestError instanceof Error ? requestError.message : 'Rynna could not complete the request');
      setMessages(displayHistory);
      setInput(prompt);
    } finally {
      setPending(false);
    }
  }

  return (
    <main className="app-shell">
      <header className="app-header">
        <div>
          <p className="eyebrow">AI software agent</p>
          <h1>Rynna</h1>
        </div>
        <div className="header-actions">
          {view === 'chat' && profiles.length > 0 ? (
            <label className="profile-picker" htmlFor="profile">
              <span>Profile</span>
              <Typeahead
                disabled={pending || deletingSession}
                id="profile"
                onChange={selectProfile}
                options={sortedProfiles(profiles).map((profile) => profile.name)}
                value={selectedProfile ?? ''}
              />
            </label>
          ) : null}
          {client.connectOpenAi ? (
            <Button
              className="account-button"
              onClick={() => {
                if (showOpenAi) setApiKey('');
                setShowOpenAi(!showOpenAi);
              }}
              type="button"
              variant="outline"
            >
              {openAiAccount?.connected
                ? openAiAccount.method === 'chatgpt'
                  ? `Connected with ChatGPT${openAiAccount.plan ? ` ${formatPlan(openAiAccount.plan)}` : ''}`
                  : 'Connected with API key'
                : 'Connect OpenAI'}
            </Button>
          ) : null}
          {canOpenSettings ? (
            <Button
              className="account-button"
              onClick={() => {
                setView(view === 'settings' ? 'chat' : 'settings');
                setEditingProvider(null);
                setError(null);
              }}
              type="button"
              variant="outline"
            >
              {view === 'settings' ? 'Back to chat' : 'Settings'}
            </Button>
          ) : null}
          <ThemeToggle />
          <span className="status"><span aria-hidden="true" /> Ready</span>
        </div>
      </header>

      {view === 'chat' && showOpenAi && client.connectOpenAi ? (
        <section className="account-panel" aria-label="Connect OpenAI">
          <Button
            disabled={connectingOpenAi}
            onClick={() => void connectOpenAi('chatgpt')}
            type="button"
            variant="secondary"
          >
            {connectingOpenAi ? 'Connecting…' : 'Use ChatGPT subscription'}
          </Button>
          <span>or</span>
          <label htmlFor="openai-api-key">OpenAI API key</label>
          <Input
            autoComplete="off"
            id="openai-api-key"
            onChange={(event) => setApiKey(event.target.value)}
            type="password"
            value={apiKey}
          />
          <Button
            disabled={connectingOpenAi || !apiKey.trim()}
            onClick={() => void connectOpenAi('api_key')}
            type="button"
          >
            Save API key
          </Button>
        </section>
      ) : null}

      {view === 'chat' ? (
        <div className="chat-workspace">
          <SessionSidebar
            activeSessionId={activeSessionId}
            disabled={pending || deletingSession}
            onNewSession={() => startNewSession(project)}
            onSelectProject={startNewSession}
            onSelectSession={selectSession}
            onDeleteSession={removeSession}
            profile={selectedProfile ?? ''}
            projects={activeProfile?.projects ?? []}
            sessions={sessions}
          />
          <div className="chat-main">
            {activeProfile ? (
              <aside className="profile-summary" aria-label="Active profile">
                {activeProfile.active_skills.map((skill) => (
                  <Badge key={`skill-${skill}`}>{skill} skill</Badge>
                ))}
                {activeProfile.mcp_servers.map((server) => (
                  <Badge key={`mcp-${server}`}>{server} MCP</Badge>
                ))}
                {activeProfile.capabilities.map((capability) => (
                  <Badge key={`capability-${capability}`}>{capability} capability</Badge>
                ))}
                <Badge>{project ?? 'Default project'} · {project
                  ? activeProfile.projects.find(candidate => candidate.name === project)?.default_directory
                  : activeProfile.default_project_directory}</Badge>
              </aside>
            ) : null}
            <section className="conversation" aria-label="Conversation">
              {activeProfile && client.startWorkflow && client.listWorkflowRuns ? <WorkflowPanel
                key={`${activeProfile.name}:${workflowSession}`} client={client} profile={activeProfile.name} session={workflowSession}
                savedRunId={sessions.find(s => s.id === workflowSession)?.workflow_run_id} project={project ?? null} selected={selectedWorkflow} context={conversationHistory(messages).map(m => `${m.role}: ${m.content}`).join('\n')}
                selection={selection ?? { provider: (activeProfile.providers.find(p => p.enabled !== false && p.default) ?? activeProfile.providers.find(p => p.enabled !== false))?.provider ?? '', model: (activeProfile.providers.find(p => p.enabled !== false && p.default) ?? activeProfile.providers.find(p => p.enabled !== false))?.model ?? '', thinking: 'default' }}
                onSelection={saveWorkflowSelection} onRun={receiveWorkflow} /> : null}
              <div className="messages" role="log" aria-live="polite">
                {messages.length === 0 ? (
                  <div className="empty-state">
                    <p className="thread-mark" aria-hidden="true">A</p>
                    <h2>What should we work through?</h2>
                    <p>Ask Rynna to investigate, plan, or execute a development task.</p>
                  </div>
                ) : (
                  messages.map((message, index) =>
                    message.role === 'thinking' ? (
                      <details
                        className="thinking-block"
                        key={`thinking-${index}`}
                        open={message.expanded}
                        onToggle={(event) => {
                          const expanded = event.currentTarget.open;
                          setMessages((current) =>
                            current.map((candidate, candidateIndex) =>
                              candidateIndex === index && candidate.role === 'thinking'
                                ? { ...candidate, expanded }
                                : candidate,
                            ),
                          );
                        }}
                      >
                        <summary>Thinking</summary>
                        <p>{message.content}</p>
                      </details>
                    ) : (
                      <article className={`message message-${message.role}`} key={`${message.role}-${index}`}>
                        <p className="message-role">{message.role === 'assistant' ? 'Rynna' : 'You'}</p>
                        <p>{message.content}</p>
                      </article>
                    ),
                  )
                )}
              </div>

              {error ? <p className="request-error" role="alert">{error}</p> : null}
              <form className="composer" onSubmit={submit}>
                <label htmlFor="prompt">Message Rynna</label>
                <div className="composer-row">
                  <Textarea
                    id="prompt"
                    name="prompt"
                    value={input}
                    onChange={(event) => setInput(event.target.value)}
                    onKeyDown={(event) => {
                      if (
                        event.key === 'Enter' &&
                        !event.shiftKey &&
                        !event.altKey &&
                        !event.ctrlKey &&
                        !event.metaKey &&
                        !event.nativeEvent.isComposing
                      ) {
                        event.preventDefault();
                        event.currentTarget.form?.requestSubmit();
                      }
                    }}
                    placeholder="Describe the task, constraints, and desired outcome…"
                    rows={3}
                  />
                </div>
                <div className="composer-actions">
                  {activeProfile ? <ModelSelector profile={activeProfile} selection={selection} disabled={pending || deletingSession}
                    onChange={value => setChatSelection({ profile: activeProfile.name, value })} /> : null}
                  <Button disabled={pending || workflowRunning || deletingSession || !input.trim()} type="submit">
                    {workflowRunning ? 'Use workflow steering above' : pending ? 'Working…' : 'Send'}
                  </Button>
                </div>
              </form>
            </section>
          </div>
        </div>
      ) : null}

      {view === 'settings' ? (
        <section className="settings-page" aria-label="Settings">
          <aside className="settings-sidebar">
            <p className="eyebrow">Settings</p>
            <nav aria-label="Settings">
              {canEditProfiles ? (
                <Button
                  aria-current={settingsSection === 'profiles' ? 'page' : undefined}
                  onClick={() => setSettingsSection('profiles')}
                  type="button"
                  variant="ghost"
                >
                  Profiles
                </Button>
              ) : null}
              {client.updateProfile && configuredProfiles.length > 0 ? (
                <Button
                  aria-current={settingsSection === 'projects' ? 'page' : undefined}
                  onClick={() => setSettingsSection('projects')}
                  type="button"
                  variant="ghost"
                >
                  Projects
                </Button>
              ) : null}
              {client.listProviders ? (
                <Button
                  aria-current={settingsSection === 'provider-credentials' ? 'page' : undefined}
                  onClick={() => setSettingsSection('provider-credentials')}
                  type="button"
                  variant="ghost"
                >
                  Provider credentials
                </Button>
              ) : null}
              {client.updateProfile && configuredProfiles.length > 0 ? (
                <Button
                  aria-current={settingsSection === 'models' ? 'page' : undefined}
                  onClick={() => setSettingsSection('models')}
                  type="button"
                  variant="ghost"
                >
                  Models
                </Button>
              ) : null}
              {client.getMcpSettings ? (
                <Button aria-current={settingsSection === 'mcp' ? 'page' : undefined}
                  onClick={() => setSettingsSection('mcp')} type="button" variant="ghost">
                  MCP servers
                </Button>
              ) : null}
              {client.updateProfile ? (
                <Button aria-current={settingsSection === 'subagents' ? 'page' : undefined}
                  onClick={() => setSettingsSection('subagents')} type="button" variant="ghost">
                  Subagents
                </Button>
              ) : null}
              {client.listWorkflows ? <Button aria-current={settingsSection === 'workflows' ? 'page' : undefined} onClick={() => setSettingsSection('workflows')} type="button" variant="ghost">Workflows</Button> : null}
              {client.getMemorySettings ? (
                <Button aria-current={settingsSection === 'memory' ? 'page' : undefined}
                  onClick={() => setSettingsSection('memory')} type="button" variant="ghost">
                  Memory provider
                </Button>
              ) : null}
            </nav>
          </aside>
          <div className="settings-content">
            {settingsSection === 'projects' && client.updateProfile ? (
              <>
                <label className="profile-picker" htmlFor="projects-profile">
                  <span>Profile</span>
                  <Typeahead
                    id="projects-profile"
                    onChange={selectSettingsProfile}
                    options={sortedProfiles(configuredProfiles).map(profile => profile.name)}
                    value={selectedSettingsProfile ?? ''}
                  />
                </label>
                {activeConfiguredProfile ? (
                  <ProjectSettings
                    key={activeConfiguredProfile.name}
                    client={client}
                    onSaved={saved => {
                      setSessions(current => reconcileProjectSessions(
                        current,
                        saved.name,
                        activeConfiguredProfile.projects.map(project => project.name),
                        saved.projects.map(project => project.name),
                      ));
                      setConfiguredProfiles(current => current.map(profile => profile.name === saved.name ? saved : profile));
                      setProfiles(current => current.map(profile => profile.name === saved.name ? {
                        ...profile,
                        default_project_directory: saved.default_project_directory,
                        projects: saved.projects,
                      } : profile));
                      if (selectedProfile === saved.name) {
                        if (chatProject?.name && !saved.projects.some(candidate => candidate.name === chatProject.name)) {
                          setChatProject(undefined);
                        }
                        setMessages([]);
                        sessionId.current = null;
    workflowDraftId.current = newSessionId();
    setWorkflowRunning(false);
                        setActiveSessionId(null);
                      }
                    }}
                    profile={activeConfiguredProfile}
                  />
                ) : <p>Select a profile to configure its projects.</p>}
              </>
            ) : null}
            {settingsSection === 'workflows' && client.listWorkflows ? <>
              <label className="profile-picker">Profile<select value={selectedSettingsProfile ?? ''} onChange={e => selectSettingsProfile(e.target.value)}>{configuredProfiles.map(p => <option key={p.name}>{p.name}</option>)}</select></label>
              {activeConfiguredProfile && <WorkflowSettings key={activeConfiguredProfile.name} client={client} profile={activeConfiguredProfile} />}
            </> : null}
            {settingsSection === 'subagents'  && client.updateProfile ? (
              <>
                <label className="profile-picker" htmlFor="subagents-profile">
                  <span>Profile</span>
                  <Typeahead id="subagents-profile" onChange={selectSettingsProfile}
                    options={sortedProfiles(configuredProfiles).map(profile => profile.name)}
                    value={selectedSettingsProfile ?? ''} />
                </label>
                {activeConfiguredProfile ? (
                  <SubagentSettings key={activeConfiguredProfile.name} client={client}
                    profile={activeConfiguredProfile}
                    onSaved={saved => {
                      setConfiguredProfiles(current => current.map(profile => profile.name === saved.name ? saved : profile));
                      setProfiles(current => current.map(profile => profile.name === saved.name ? { ...profile, subagents: saved.subagents } : profile));
                    }} />
                ) : <p>Select a profile to configure its subagents.</p>}
              </>
            ) : null}
            {settingsSection === 'mcp' ? (
              <>
                <label className="profile-picker" htmlFor="mcp-profile">
                  <span>Profile</span>
                  <select
                    id="mcp-profile"
                    value={selectedSettingsProfile ?? ''}
                    onChange={(event) => selectSettingsProfile(event.target.value)}
                  >
                    {!selectedSettingsProfile ? <option value="" disabled>Select a profile</option> : null}
                    {sortedProfiles([
                      ...configuredProfiles,
                      ...profiles.filter(
                        (profile) => profile.name === 'openai-account' &&
                          !configuredProfiles.some((saved) => saved.name === profile.name),
                      ),
                    ]).map((profile) => (
                      <option key={profile.name} value={profile.name}>{profile.name}</option>
                    ))}
                  </select>
                </label>
                {selectedSettingsProfile ? (
                  <McpSettingsPanel key={selectedSettingsProfile} client={client} profile={selectedSettingsProfile} />
                ) : <p>Select a profile to configure its MCP servers.</p>}
              </>
            ) : null}
            {settingsSection === 'memory' ? (
              <>
                <label className="profile-picker" htmlFor="memory-profile">
                  <span>Profile</span>
                  <select
                    id="memory-profile"
                    value={selectedSettingsProfile ?? ''}
                    onChange={(event) => selectSettingsProfile(event.target.value)}
                  >
                    {!selectedSettingsProfile ? <option value="" disabled>Select a profile</option> : null}
                    {sortedProfiles([
                      ...configuredProfiles,
                      ...profiles.filter(
                        (profile) => profile.name === 'openai-account' &&
                          !configuredProfiles.some((saved) => saved.name === profile.name),
                      ),
                    ]).map((profile) => (
                      <option key={profile.name} value={profile.name}>{profile.name}</option>
                    ))}
                  </select>
                </label>
                {selectedSettingsProfile ? (
                  <MemorySettingsPanel key={selectedSettingsProfile} client={client} profile={selectedSettingsProfile} />
                ) : <p>Select a profile to configure its memory provider.</p>}
              </>
            ) : null}
            {settingsSection === 'profiles' && canEditProfiles ? (
              <>
                <div className="settings-heading">
                  <div>
                    <h2>Profiles</h2>
                    <p>Create, rename, and delete the profiles available in Rynna.</p>
                  </div>
                  <div className="provider-actions">
                    <Button disabled={savingProfile} onClick={beginAddProfile} type="button">
                      Add profile
                    </Button>
                    <Button
                      disabled={savingProfile || addingProfile || configuredProfiles.length <= 1}
                      onClick={() => void removeProfile()}
                      type="button"
                      variant="ghost"
                    >
                      Delete profile
                    </Button>
                  </div>
                </div>
                {configuredProfiles.length === 0 && !addingProfile ? (
                  <p className="settings-empty">No profiles configured.</p>
                ) : (
                  <>
                    {addingProfile ? null : (
                      <label className="profile-picker" htmlFor="settings-profile">
                        <span>Profile</span>
                        <Typeahead
                          disabled={savingProfile}
                          id="settings-profile"
                          onChange={selectSettingsProfile}
                          options={sortedProfiles(configuredProfiles).map((profile) => profile.name)}
                          value={selectedSettingsProfile ?? ''}
                        />
                      </label>
                    )}
                    <form className="provider-form profile-form" onSubmit={(event) => void saveProfile(event)}>
                      <h3>{addingProfile ? 'Add profile' : 'Edit profile'}</h3>
                      <label htmlFor="profile-name">Name</label>
                      <Input
                        id="profile-name"
                        onChange={(event) => setProfileName(event.target.value)}
                        required
                        value={profileName}
                      />
                      <label htmlFor="profile-skills">Skills</label>
                      <Textarea
                        aria-describedby="profile-skills-help"
                        disabled={savingProfile}
                        id="profile-skills"
                        onChange={(event) => setProfileSkills(event.target.value)}
                        rows={4}
                        value={profileSkills}
                      />
                      <p id="profile-skills-help">
                        One skill name or directory per line. Each directory must contain SKILL.md
                        on the machine running Rynna. Leave empty for no skills. Restart Rynna after saving.
                      </p>
                      <div className="provider-actions">
                        <Button
                          disabled={
                            savingProfile ||
                            !profileName.trim()
                          }
                          type="submit"
                        >
                          Save profile
                        </Button>
                        {addingProfile ? (
                          <Button onClick={() => setAddingProfile(false)} type="button" variant="ghost">
                            Cancel
                          </Button>
                        ) : null}
                      </div>
                    </form>
                  </>
                )}
              </>
            ) : null}
            {settingsSection === 'provider-credentials' && client.listProviders ? (
              <>
                <div className="settings-heading">
                  <div>
                    <h2>Provider credentials</h2>
                    <p>Manage the provider authentication used by the selected profile.</p>
                  </div>
                  <Button disabled={providerSettings.length >= 3} onClick={beginAddProvider} type="button">
                    Add provider
                  </Button>
                </div>
                {configuredProfiles.length > 0 ? (
                  <label className="profile-picker" htmlFor="credentials-profile">
                    <span>Profile</span>
                    <Typeahead
                      disabled={savingProvider}
                      id="credentials-profile"
                      onChange={selectSettingsProfile}
                      options={sortedProfiles(configuredProfiles).map((profile) => profile.name)}
                      value={selectedSettingsProfile ?? ''}
                    />
                  </label>
                ) : null}
                <p>
                  Credentials are isolated by profile. Runtime profile and model changes take effect after restart.
                </p>
          {providerSettings.length === 0 ? (
            <p className="settings-empty">No providers configured.</p>
          ) : (
            <div className="provider-list">
              {[...providerSettings]
                .sort((left, right) =>
                  providerTitle(left.kind).localeCompare(providerTitle(right.kind)),
                )
                .map((provider) => (
                <article className="provider-card" key={provider.kind}>
                  <div>
                    <h3>{providerTitle(provider.kind)}</h3>
                    <p>
                      {provider.kind === 'ollama' || provider.kind === 'mlx'
                        ? provider.api_base
                        : provider.kind === 'openrouter'
                          ? 'API key via OPENROUTER_API_KEY'
                        : provider.kind === 'anthropic'
                          ? provider.authentication === 'subscription'
                            ? 'Claude subscription / usage bundle'
                            : 'API key via environment variable'
                          : provider.authentication === 'chatgpt'
                            ? 'ChatGPT subscription'
                            : 'API key'}
                    </p>
                  </div>
                  <div className="provider-actions">
                    <Button
                      aria-label={`Edit ${providerTitle(provider.kind)}`}
                      onClick={() => beginEditProvider(provider)}
                      size="sm"
                      type="button"
                      variant="outline"
                    >
                      Edit
                    </Button>
                    <Button
                      aria-label={`Delete ${providerTitle(provider.kind)}`}
                      onClick={() => void removeProvider(provider.kind)}
                      size="sm"
                      type="button"
                      variant="ghost"
                    >
                      Delete
                    </Button>
                  </div>
                </article>
                ))}
            </div>
          )}
          {editingProvider ? (
            <form className="provider-form" onSubmit={(event) => void saveProvider(event)}>
              <h3>
                {providerSettings.some((provider) => provider.kind === editingProvider)
                  ? 'Edit provider'
                  : 'Add provider'}
              </h3>
              <label htmlFor="provider-type">Provider type</label>
              {providerSettings.some((provider) => provider.kind === editingProvider) ? (
                <select disabled id="provider-type" value={providerKind}>
                  <option value={providerKind}>{providerTitle(providerKind)}</option>
                </select>
              ) : (
                <div className="provider-typeahead">
                  <Input
                    aria-autocomplete="list"
                    aria-activedescendant={
                      providerTypeOpen && activeProviderKind
                        ? `provider-type-option-${activeProviderKind}`
                        : undefined
                    }
                    aria-controls="provider-type-options"
                    aria-expanded={providerTypeOpen}
                    autoComplete="off"
                    id="provider-type"
                    onBlur={() => {
                      setProviderTypeQuery(providerTitle(providerKind));
                      setProviderTypeOpen(false);
                      setProviderTypeDirty(false);
                      setActiveProviderKind(providerKind);
                    }}
                    onChange={(event) => {
                      const query = event.target.value;
                      setProviderTypeQuery(query);
                      setProviderTypeOpen(true);
                      setProviderTypeDirty(true);
                      setActiveProviderKind(matchingProviderKinds(query, false)[0] ?? null);
                    }}
                    onFocus={(event) => {
                      event.currentTarget.select();
                      setProviderTypeOpen(true);
                      const matches = matchingProviderKinds(providerTypeQuery);
                      setActiveProviderKind(matches.includes(providerKind) ? providerKind : (matches[0] ?? null));
                    }}
                    onKeyDown={(event) => {
                      if (event.key === 'Escape') {
                        setProviderTypeQuery(providerTitle(providerKind));
                        setProviderTypeOpen(false);
                        setProviderTypeDirty(false);
                        setActiveProviderKind(providerKind);
                        return;
                      }
                      const matches = matchingProviderKinds(providerTypeQuery);
                      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                        if (matches.length === 0) return;
                        event.preventDefault();
                        setProviderTypeOpen(true);
                        const currentIndex = activeProviderKind ? matches.indexOf(activeProviderKind) : -1;
                        const offset = event.key === 'ArrowDown' ? 1 : -1;
                        const nextIndex =
                          currentIndex === -1
                            ? event.key === 'ArrowDown' ? 0 : matches.length - 1
                            : (currentIndex + offset + matches.length) % matches.length;
                        setActiveProviderKind(matches[nextIndex] ?? null);
                        return;
                      }
                      if (event.key !== 'Enter') return;
                      const match =
                        (activeProviderKind && matches.includes(activeProviderKind)
                          ? activeProviderKind
                          : matches[0]) ?? null;
                      if (!match) return;
                      event.preventDefault();
                      selectProviderKind(match);
                    }}
                    role="combobox"
                    value={providerTypeQuery}
                  />
                  {providerTypeOpen ? (
                    <div className="provider-type-options" id="provider-type-options" role="listbox">
                      {matchingProviderKinds(providerTypeQuery).map((kind) => (
                        <button
                          aria-selected={kind === activeProviderKind}
                          id={`provider-type-option-${kind}`}
                          key={kind}
                          onClick={() => selectProviderKind(kind)}
                          onMouseDown={(event) => event.preventDefault()}
                          onMouseMove={() => setActiveProviderKind(kind)}
                          role="option"
                          tabIndex={-1}
                          type="button"
                        >
                          {providerTitle(kind)}
                        </button>
                      ))}
                    </div>
                  ) : null}
                </div>
              )}
              {providerKind === 'ollama' || providerKind === 'mlx' ? (
                <>
                  <label htmlFor="local-api-base">{providerTitle(providerKind)} API base URL</label>
                  <Input
                    id="local-api-base"
                    onChange={(event) => providerKind === 'mlx' ? setMlxApiBase(event.target.value) : setOllamaApiBase(event.target.value)}
                    required
                    type="url"
                    value={providerKind === 'mlx' ? mlxApiBase : ollamaApiBase}
                  />
                </>
              ) : providerKind === 'openai' ? (
                <>
                  <label htmlFor="openai-authentication">OpenAI authentication</label>
                  <select
                    id="openai-authentication"
                    onChange={(event) => {
                      setOpenAiAuthentication(event.target.value as 'api_key' | 'chatgpt');
                      setReuseExistingChatgpt(null);
                    }}
                    value={openAiAuthentication}
                  >
                    <option value="chatgpt">ChatGPT subscription</option>
                    <option value="api_key">API key</option>
                  </select>
                  {openAiAuthentication === 'api_key' ? (
                    <>
                      <label htmlFor="provider-openai-api-key">OpenAI API key</label>
                      <Input
                        autoComplete="off"
                        id="provider-openai-api-key"
                        onChange={(event) => setProviderApiKey(event.target.value)}
                        required
                        type="password"
                        value={providerApiKey}
                      />
                    </>
                  ) : existingOpenAiAccount?.connected && existingOpenAiAccount.method === 'chatgpt' ? (
                    <div className="credential-choice">
                      <strong>Existing ChatGPT credentials found</strong>
                      <p>
                        {existingOpenAiAccount.plan
                          ? `ChatGPT ${formatPlan(existingOpenAiAccount.plan)}`
                          : 'A ChatGPT subscription'} is already connected. Use it or sign in with a
                        different account for Rynna.
                      </p>
                      <div className="provider-actions">
                        <Button
                          aria-pressed={reuseExistingChatgpt === true}
                          onClick={() => setReuseExistingChatgpt(true)}
                          type="button"
                          variant="outline"
                        >
                          Use existing credentials
                        </Button>
                        <Button
                          aria-pressed={reuseExistingChatgpt === false}
                          onClick={() => setReuseExistingChatgpt(false)}
                          type="button"
                          variant="outline"
                        >
                          Register new credentials
                        </Button>
                      </div>
                      {reuseExistingChatgpt === false ? (
                        <p>A browser window will open so you can sign in to ChatGPT.</p>
                      ) : null}
                    </div>
                  ) : (
                    <p>A browser window will open so you can sign in to ChatGPT.</p>
                  )}
                </>
              ) : providerKind === 'openrouter' ? (
                <p>
                  Set OPENROUTER_API_KEY in the Rynna process environment; the key is never saved
                  in provider settings.
                </p>
              ) : (
                <>
                  <label htmlFor="anthropic-authentication">Anthropic authentication</label>
                  <select
                    id="anthropic-authentication"
                    onChange={(event) =>
                      setAnthropicAuthentication(event.target.value as 'api_key' | 'subscription')
                    }
                    value={anthropicAuthentication}
                  >
                    <option value="subscription">Claude subscription / usage bundle</option>
                    <option value="api_key">API key from ANTHROPIC_API_KEY</option>
                  </select>
                  <p>
                    {anthropicAuthentication === 'subscription'
                      ? 'A browser window will open for Claude login. Rynna tools are disabled for this mode.'
                      : 'Set ANTHROPIC_API_KEY in the Rynna process environment; the key is never saved in provider settings.'}
                  </p>
                </>
              )}
              <div className="provider-actions">
                <Button
                  disabled={
                    savingProvider ||
                    providerTypeQuery !== providerTitle(providerKind) ||
                    (providerKind === 'openai' &&
                      openAiAuthentication === 'chatgpt' &&
                      (discoveringExistingOpenAiAccount ||
                        (existingOpenAiAccount?.connected === true &&
                          existingOpenAiAccount.method === 'chatgpt' &&
                          reuseExistingChatgpt === null)))
                  }
                  type="submit"
                >
                  Save provider
                </Button>
                <Button
                  onClick={() => {
                    setEditingProvider(null);
                    setReuseExistingChatgpt(null);
                    setProviderApiKey('');
                  }}
                  type="button"
                  variant="ghost"
                >
                  Cancel
                </Button>
              </div>
            </form>
          ) : null}
              </>
            ) : null}
          {settingsSection === 'models' && client.updateProfile && activeConfiguredProfile ? (
            <>
              <div className="settings-heading">
                <div>
                  <h2>Models</h2>
                  <p>Saved model changes take effect after restart. Chat uses the currently running models until then.</p>
                  <p>Add the exact model name your provider serves, then choose which models are available in chat.</p>
                </div>
              </div>
              <div className="model-filters">
                <label className="profile-picker" htmlFor="models-profile">
                  <span>Profile</span>
                  <Typeahead
                    disabled={savingProfile}
                    id="models-profile"
                    onChange={selectSettingsProfile}
                    options={sortedProfiles(configuredProfiles).map((profile) => profile.name)}
                    value={selectedSettingsProfile ?? ''}
                  />
                </label>
                <label className="profile-picker" htmlFor="models-provider">
                  <span>Provider</span>
                  <Typeahead
                    disabled={savingProfile}
                    id="models-provider"
                    onChange={setModelProvider}
                    options={[...catalogProviderIds].sort((left, right) => left.localeCompare(right))}
                    value={modelProvider}
                  />
                </label>
              </div>
              <form className="provider-form" onSubmit={(event) => void addCustomModel(event)}>
                <label htmlFor="custom-model-name">Model name</label>
                <Input
                  id="custom-model-name"
                  value={customModel}
                  onChange={(event) => setCustomModel(event.target.value)}
                  disabled={savingProfile || !modelProvider}
                  placeholder={modelProvider === 'mlx' ? 'mlx-community/Qwen3.8-27B-8bit' : 'e.g. qwen3:14b'}
                  required
                />
                <div className="provider-actions">
                  <Button type="submit" disabled={savingProfile || !modelProvider || !customModel.trim()}>
                    Add model
                  </Button>
                </div>
              </form>
              <div className="model-toolbar" aria-label="Model bulk actions">
                <div className="provider-actions">
                  <Button
                    onClick={() =>
                      setSelectedModels(
                        activeConfiguredProfile.providers
                          .filter((provider) => provider.provider === modelProvider)
                          .map((provider) => provider.model),
                      )
                    }
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    Select all
                  </Button>
                  <Button
                    onClick={() => setSelectedModels([])}
                    size="sm"
                    type="button"
                    variant="ghost"
                  >
                    Deselect all
                  </Button>
                </div>
                <div className="provider-actions">
                  <Button
                    disabled={savingProfile || selectedModels.length === 0}
                    onClick={() => setSelectedModelState(true)}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    Enable selected
                  </Button>
                  <Button
                    disabled={
                      savingProfile ||
                      selectedModels.length === 0 ||
                      disablingWouldRemoveEveryEnabledModel
                    }
                    onClick={() => setSelectedModelState(false)}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    Disable selected
                  </Button>
                </div>
              </div>
              <div className="model-list">
                {activeConfiguredProfile.providers
                  .filter((provider) => provider.provider === modelProvider)
                  .map((provider, index) => {
                    const enabled = provider.enabled !== false;
                    const hasExplicitDefault = activeConfiguredProfile.providers.some(
                      (candidate) => candidate.default,
                    );
                    const isDefault = provider.default === true || (!hasExplicitDefault && index === 0);
                    return (
                      <article className="model-row" key={`${provider.provider}-${provider.model}`}>
                        <label>
                          <input
                            aria-label={`Select ${provider.model}`}
                            checked={selectedModels.includes(provider.model)}
                            onChange={(event) =>
                              setSelectedModels((current) =>
                                event.target.checked
                                  ? [...current, provider.model]
                                  : current.filter((model) => model !== provider.model),
                              )
                            }
                            type="checkbox"
                          />
                          <span>
                            <strong>{provider.model}</strong>
                            <small>{enabled ? 'Enabled in saved profile' : 'Disabled'}</small>
                          </span>
                        </label>
                        <label className="default-model-control">
                          <input
                            aria-label={`Make ${provider.model} default`}
                            checked={isDefault}
                            disabled={savingProfile}
                            name="default-model"
                            onChange={() => setDefaultModel(provider.model)}
                            type="radio"
                          />
                          Default
                        </label>
                      </article>
                    );
                  })}
              </div>
            </>
          ) : null}
          {error ? <p className="request-error" role="alert">{error}</p> : null}
          </div>
        </section>
      ) : null}

    </main>
  );
}

function formatPlan(plan: string): string {
  return plan.replaceAll('_', ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function providerTitle(kind: ConfiguredProvider['kind']): string {
  if (kind === 'ollama') return 'Ollama';
  if (kind === 'mlx') return 'MLX';
  if (kind === 'openai') return 'OpenAI';
  if (kind === 'openrouter') return 'OpenRouter';
  return 'Anthropic';
}
