import { useState } from 'react';
import { Wrench } from 'lucide-react';
import type { AgentClient, Profile, ToolsetId } from '../contracts';
import { Badge } from './ui/badge';
import { Button } from './ui/button';

// Names are the native Tool definitions, not aliases from other agents.
export const toolsets: { id: ToolsetId; title: string; description: string; tools: string[]; requirement: string }[] = [
  { id: 'file_operations', title: 'File Operations', description: 'read, write, patch, search', tools: ['edit_file', 'read_file', 'search_files', 'write_file', 'find_files', 'list_directory', 'create_directory', 'file_info'], requirement: 'Read/search are supplied by default. Writes, edits and directory creation require a named filesystem capability with read_only: false, selected in the profile YAML; restart after capability configuration changes. No approval dialog grants access.' },
  { id: 'code_search', title: 'Code Search', description: 'indexed code search across project repositories', tools: ['code_search'], requirement: 'Read-only project search is supplied by default unless an explicit filesystem capability or MCP provider changes its configuration.' },
  { id: 'commands', title: 'Commands', description: 'run configured host programs', tools: ['run_command'], requirement: 'Configure a named command capability with an explicit executable map, select it in the profile YAML, then restart. There is no per-call approval dialog.' },
  { id: 'skills', title: 'Skills', description: 'load selected skill instructions and resources', tools: ['read_skill'], requirement: 'Requires active skills in this profile.' },
  { id: 'subagents', title: 'Subagents', description: 'delegate tasks to profile helpers', tools: ['delegate_task'], requirement: 'Requires subagents in this profile. Helpers inherit the parent’s permitted tools.' },
];

export function ToolsetSettings({ client, profile, runtimeProfile, onSaved }: { client: AgentClient; profile: Profile; runtimeProfile?: Profile; onSaved(profile: Profile): void }) {
  const [yolo, setYolo] = useState(profile.yolo ?? false);
  const effectiveYolo = Boolean(profile.yolo || runtimeProfile?.yolo);
  const [editing, setEditing] = useState<ToolsetId | null>(null);
  const [enabled, setEnabled] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const disabled = profile.disabled_toolsets ?? [];

  function readiness(id: ToolsetId) {
    if (effectiveYolo) return 'YOLO override';
    if (disabled.includes(id)) return 'Disabled';
    if (id === 'skills') return profile.active_skills.length ? 'Needs verification' : 'Needs setup';
    if (id === 'subagents') return profile.subagents.length ? 'Ready' : 'Needs setup';
    // Capability IDs are opaque. Never infer kind, roots or read_only from their names.
    if (profile.capabilities.length || (id === 'code_search' && profile.mcp_servers.length)) return 'Needs verification';
    if (id === 'file_operations') return 'Ready · read-only';
    return id === 'code_search' ? 'Ready' : 'Needs setup';
  }

  async function saveMode() {
    if (!client.updateProfile || saving) return;
    setSaving(true); setError(null); setStatus(null);
    try {
      onSaved(await client.updateProfile(profile.name, { ...profile, yolo }));
      setEditing(null);
      setStatus(`Execution mode saved for ${profile.name}. Changes apply to the next request.`);
    } catch (error) {
      setError(error instanceof Error ? error.message : 'Rynna could not save execution mode.');
    } finally { setSaving(false); }
  }

  async function save() {
    if (!editing || !client.updateProfile || saving) return;
    setSaving(true); setError(null); setStatus(null);
    const next = disabled.filter(id => id !== editing);
    if (!enabled) next.push(editing);
    try {
      onSaved(await client.updateProfile(profile.name, { ...profile, disabled_toolsets: next }));
      setEditing(null);
      setStatus(`Toolsets saved for ${profile.name}. Changes apply to the next request.`);
    } catch (error) {
      setError(error instanceof Error ? error.message : 'Rynna could not save toolsets.');
    } finally { setSaving(false); }
  }

  return <div className="toolset-settings">
    <div className="settings-heading"><div><h2>Toolsets</h2><p>Choose which groups of tools this profile may use.</p></div></div>
    <form className="my-4 grid gap-3 rounded-xl border border-destructive/50 p-4" onSubmit={event => { event.preventDefault(); void saveMode(); }}>
      <h3 className="font-semibold">Execution mode</h3>
      <label className="flex items-center gap-2"><input type="checkbox" checked={yolo} disabled={saving || !client.updateProfile} onChange={event => setYolo(event.target.checked)} aria-describedby="yolo-warning" />Enable YOLO</label>
      <p id="yolo-warning" className="text-sm">Danger: YOLO allows native file writes and arbitrary commands, ignoring configured permissions, disabled toolsets, native timeouts and core/workflow budgets. No confirmation prompts. It can delete data, expose secrets or exhaust memory and provider quota. OS permissions, process identity, provider authentication and missing credentials still apply; no automatic privilege elevation.</p>
      <p className="text-sm">Normal mode is recommended for project read/search. YOLO retains normal-mode preferences but ignores them; it does not create missing MCP servers, skills, helpers or executables. Complete or cancel unfinished workflows before changing mode.</p>
      <Button className="justify-self-start" type="submit" disabled={saving || !client.updateProfile || yolo === (profile.yolo ?? false)}>{saving ? 'Saving execution mode…' : 'Save execution mode'}</Button>
    </form>
    {runtimeProfile?.yolo && !profile.yolo ? <p role="note">Runtime still reports YOLO despite the saved normal-mode preference. Restart without --yolo to remove a CLI override.</p> : null}
    <p>OS inspection uses run_command under existing command permissions. With no capabilities configured, read/search use the selected project (or profile default directory / process cwd). Writes and commands are not available by default. Toolset switches are preferences, not grants. A tool-capable model is required; MCP servers are managed in MCP settings.</p>
    {profile.capabilities.length > 0 ? <p>Capability names do not identify their permissions. Native tool readiness needs backend verification; no write or command access is assumed.</p> : null}
    <div className="grid gap-4 sm:grid-cols-2">
      {toolsets.map(group => <article aria-label={group.title} key={group.id} className="rounded-xl border border-border bg-card p-5 shadow-sm">
        <div className="flex flex-wrap items-center gap-3"><Wrench aria-hidden="true" className="size-5 text-primary" /><h3 className="m-0 flex-1 font-semibold">{group.title}</h3><Badge className={disabled.includes(group.id) ? '' : 'border-primary/30 bg-primary/10 text-primary'}>{readiness(group.id)}</Badge></div>
        <p className="my-3 text-sm text-muted-foreground">{group.description}</p>
        <p className="text-sm text-muted-foreground">{group.requirement}</p>
        <p className="text-xs text-muted-foreground">Tool catalog — not a list of currently available tools.</p>
        <div className="mb-4 flex flex-wrap gap-2">{group.tools.map(tool => <Badge key={tool}><code>{tool}</code></Badge>)}</div>
        <Button aria-label={`Configure ${group.title}`} disabled={saving || effectiveYolo || !client.updateProfile} onClick={() => { setEditing(group.id); setEnabled(!disabled.includes(group.id)); setError(null); setStatus(null); }} type="button" variant="outline">Configure</Button>
        {editing === group.id && !effectiveYolo ? <form className="mt-4 grid gap-3 border-t border-border pt-4" onSubmit={event => { event.preventDefault(); void save(); }}>
          <label className="flex items-center gap-2"><input type="checkbox" checked={enabled} disabled={saving} onChange={event => setEnabled(event.target.checked)} />Enable {group.title}</label>
          <div className="provider-actions"><Button type="submit" disabled={saving}>{saving ? 'Saving…' : 'Save toolset'}</Button><Button type="button" variant="ghost" disabled={saving} onClick={() => setEditing(null)}>Cancel</Button></div>
        </form> : null}
      </article>)}
    </div>
    {error ? <p className="request-error" role="alert">{error}</p> : null}
    {status ? <p role="status">{status}</p> : null}
  </div>;
}
