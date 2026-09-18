import { useState } from 'react';
import { Wrench } from 'lucide-react';
import type { AgentClient, Profile, ToolsetId } from '../contracts';
import { Badge } from './ui/badge';
import { Button } from './ui/button';

// Names are the native Tool definitions, not aliases from other agents.
export const toolsets: { id: ToolsetId; title: string; description: string; tools: string[]; requirement: string }[] = [
  { id: 'file_operations', title: 'File Operations', description: 'read, write, patch, search', tools: ['edit_file', 'read_file', 'search_files', 'write_file', 'find_files', 'list_directory', 'create_directory', 'file_info'], requirement: 'Requires a filesystem capability. Exact text patches use edit_file.' },
  { id: 'code_search', title: 'Code Search', description: 'indexed code search across project repositories', tools: ['code_search'], requirement: 'Requires a filesystem capability or an explicitly selected MCP code search provider.' },
  { id: 'commands', title: 'Commands', description: 'run configured host programs', tools: ['run_command'], requirement: 'Requires a command capability with explicitly mapped programs.' },
  { id: 'skills', title: 'Skills', description: 'load selected skill instructions and resources', tools: ['read_skill'], requirement: 'Requires active skills in this profile.' },
  { id: 'subagents', title: 'Subagents', description: 'delegate tasks to profile helpers', tools: ['delegate_task'], requirement: 'Requires subagents in this profile. Helpers inherit the parent’s permitted tools.' },
];

export function ToolsetSettings({ client, profile, onSaved }: { client: AgentClient; profile: Profile; onSaved(profile: Profile): void }) {
  const [editing, setEditing] = useState<ToolsetId | null>(null);
  const [enabled, setEnabled] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const disabled = profile.disabled_toolsets ?? [];

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
    <p>Active means permitted, not necessarily configured. Enabling a toolset never grants filesystem access or adds programs, skills, or helpers. A tool-capable model is required. MCP servers are managed in MCP settings.</p>
    <div className="grid gap-4 sm:grid-cols-2">
      {toolsets.map(group => <article aria-label={group.title} key={group.id} className="rounded-xl border border-border bg-card p-5 shadow-sm">
        <div className="flex items-center gap-3"><Wrench aria-hidden="true" className="size-5 text-primary" /><h3 className="m-0 flex-1 font-semibold">{group.title}</h3><Badge className={disabled.includes(group.id) ? '' : 'border-primary/30 bg-primary/10 text-primary'}>{disabled.includes(group.id) ? 'Disabled' : 'Active'}</Badge></div>
        <p className="my-3 text-sm text-muted-foreground">{group.description}</p>
        <div className="mb-4 flex flex-wrap gap-2">{group.tools.map(tool => <Badge key={tool}><code>{tool}</code></Badge>)}</div>
        <Button aria-label={`Configure ${group.title}`} disabled={saving} onClick={() => { setEditing(group.id); setEnabled(!disabled.includes(group.id)); setError(null); setStatus(null); }} type="button" variant="outline">Configure</Button>
        {editing === group.id ? <form className="mt-4 grid gap-3 border-t border-border pt-4" onSubmit={event => { event.preventDefault(); void save(); }}>
          <p className="text-sm text-muted-foreground">{group.requirement}</p>
          <label className="flex items-center gap-2"><input type="checkbox" checked={enabled} disabled={saving} onChange={event => setEnabled(event.target.checked)} />Enable {group.title}</label>
          <div className="provider-actions"><Button type="submit" disabled={saving}>{saving ? 'Saving…' : 'Save toolset'}</Button><Button type="button" variant="ghost" disabled={saving} onClick={() => setEditing(null)}>Cancel</Button></div>
        </form> : null}
      </article>)}
    </div>
    {error ? <p className="request-error" role="alert">{error}</p> : null}
    {status ? <p role="status">{status}</p> : null}
  </div>;
}
