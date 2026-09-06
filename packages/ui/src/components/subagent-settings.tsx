import { FormEvent, useState } from 'react';

import type { AgentClient, Profile, Subagent } from '../contracts';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';

interface SubagentSettingsProps {
  client: AgentClient;
  profile: Profile;
  onSaved(profile: Profile): void;
}

export function SubagentSettings({ client, profile, onSaved }: SubagentSettingsProps) {
  const [editingName, setEditingName] = useState<string | null>(null);
  const [draft, setDraft] = useState<Subagent>({ name: '', description: '', instructions: '' });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  function edit(helper?: Subagent) {
    setEditingName(helper?.name ?? '');
    setDraft(helper ?? { name: '', description: '', instructions: '' });
    setError(null);
    setStatus(null);
  }

  async function save(subagents: Subagent[]) {
    if (!client.updateProfile || saving) return;
    setSaving(true);
    setError(null);
    setStatus(null);
    try {
      onSaved(await client.updateProfile(profile.name, { ...profile, subagents }));
      setEditingName(null);
      setStatus(`Subagents saved for ${profile.name}. Changes apply to the next request.`);
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : 'Rynna could not save subagents.');
    } finally {
      setSaving(false);
    }
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const helper = { name: draft.name.trim(), description: draft.description.trim(), instructions: draft.instructions.trim() };
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(helper.name) || !helper.description || !helper.instructions) {
      setError('Enter a name using letters, digits, underscores or hyphens, plus a description and instructions.');
      return;
    }
    if (profile.subagents.some(candidate => candidate.name === helper.name && candidate.name !== editingName)) {
      setError('A subagent with this name already exists in this profile.');
      return;
    }
    void save(editingName
      ? profile.subagents.map(candidate => candidate.name === editingName ? helper : candidate)
      : [...profile.subagents, helper]);
  }

  return (
    <div className="project-settings subagent-settings">
      <div className="settings-heading">
        <div>
          <h2>Subagents</h2>
          <p>Give this profile specialized helpers for delegated tasks.</p>
        </div>
        <Button disabled={saving || editingName !== null || profile.subagents.length >= 32} onClick={() => edit()} type="button">Add subagent</Button>
      </div>
      <p>Helpers use the conversation’s model, project and permitted tools. Each starts with fresh history and returns its answer to the parent. Helpers cannot delegate further. A model that supports tools is required.</p>
      {profile.subagents.length === 0 ? (
        <p className="settings-empty">No subagents in this profile. Add a helper and describe when the agent should use it.</p>
      ) : (
        <div className="project-list">
          {profile.subagents.map(helper => (
            <article className="project-row" key={helper.name}>
              <div><h3>{helper.name}</h3><p>{helper.description}</p></div>
              <div className="provider-actions">
                <Button aria-label={`Edit ${helper.name}`} disabled={saving} onClick={() => edit(helper)} type="button" variant="outline">Edit</Button>
                <Button aria-label={`Delete ${helper.name}`} disabled={saving} onClick={() => void save(profile.subagents.filter(candidate => candidate.name !== helper.name))} type="button" variant="ghost">Delete</Button>
              </div>
            </article>
          ))}
        </div>
      )}
      {editingName !== null ? (
        <form className="provider-form" onSubmit={submit}>
          <h3>{editingName ? 'Edit subagent' : 'Add subagent'}</h3>
          <label htmlFor="subagent-name">Name</label>
          <Input disabled={saving} id="subagent-name" maxLength={64} onChange={event => setDraft({ ...draft, name: event.target.value })} required value={draft.name} />
          <label htmlFor="subagent-description">When to use this helper</label>
          <Textarea disabled={saving} id="subagent-description" maxLength={1024} onChange={event => setDraft({ ...draft, description: event.target.value })} placeholder="Review code changes for correctness and missing tests." required rows={2} value={draft.description} />
          <label htmlFor="subagent-instructions">Instructions</label>
          <Textarea disabled={saving} id="subagent-instructions" maxLength={32000} onChange={event => setDraft({ ...draft, instructions: event.target.value })} placeholder="Inspect the requested changes. Report actionable issues with file references and suggested fixes." required rows={6} value={draft.instructions} />
          <div className="provider-actions">
            <Button disabled={saving} type="submit">{saving ? 'Saving…' : 'Save subagent'}</Button>
            <Button disabled={saving} onClick={() => { setEditingName(null); setError(null); }} type="button" variant="ghost">Cancel</Button>
          </div>
        </form>
      ) : null}
      {error ? <p className="request-error" role="alert">{error}</p> : null}
      {status ? <p role="status">{status}</p> : null}
    </div>
  );
}
