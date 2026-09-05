import { FormEvent, useEffect, useState } from 'react';

import type { AgentClient, Profile, Workspace } from '../contracts';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';

interface WorkspaceSettingsProps {
  client: AgentClient;
  profile: Profile;
  onSaved(profile: Profile): void;
}

export function WorkspaceSettings({ client, profile, onSaved }: WorkspaceSettingsProps) {
  const [startingDirectory, setStartingDirectory] = useState(profile.default_workspace_directory);
  const [editingName, setEditingName] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [directories, setDirectories] = useState('');
  const [defaultDirectory, setDefaultDirectory] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setStartingDirectory(profile.default_workspace_directory);
    cancelEdit();
  }, [profile.name, profile.default_workspace_directory]);

  async function save(next: Profile): Promise<boolean> {
    if (!client.updateProfile || saving) return false;
    setSaving(true);
    setError(null);
    try {
      onSaved(await client.updateProfile(profile.name, next));
      return true;
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : 'Rynna could not save workspace settings');
      return false;
    } finally {
      setSaving(false);
    }
  }

  function beginEdit(workspace?: Workspace) {
    setEditingName(workspace?.name ?? '');
    setName(workspace?.name ?? '');
    setDirectories(workspace?.directories.join('\n') ?? '');
    setDefaultDirectory(workspace?.default_directory ?? '');
    setError(null);
  }

  function cancelEdit() {
    setEditingName(null);
    setName('');
    setDirectories('');
    setDefaultDirectory('');
    setError(null);
  }

  async function saveDefaultWorkspace(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = startingDirectory.trim();
    if (!value) return;
    await save({ ...profile, default_workspace_directory: value });
  }

  async function saveWorkspace(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const nextDirectories = directories.split('\n').map(value => value.trim()).filter(Boolean);
    const workspace: Workspace = {
      name: name.trim(),
      directories: nextDirectories,
      default_directory: defaultDirectory.trim(),
    };
    if (!workspace.name || nextDirectories.length === 0 || !nextDirectories.includes(workspace.default_directory)) {
      setError('Enter a name and choose a default directory from the workspace directories.');
      return;
    }
    const workspaces = editingName
      ? profile.workspaces.map(candidate => candidate.name === editingName ? workspace : candidate)
      : [...profile.workspaces, workspace];
    if (await save({ ...profile, workspaces })) cancelEdit();
  }

  async function removeWorkspace(workspaceName: string) {
    const removed = await save({
      ...profile,
      workspaces: profile.workspaces.filter(workspace => workspace.name !== workspaceName),
    });
    if (removed && editingName === workspaceName) cancelEdit();
  }

  const directoryOptions = directories.split('\n').map(value => value.trim()).filter(Boolean);

  return (
    <div className="workspace-settings">
      <div className="settings-heading">
        <div>
          <h2>Workspaces</h2>
          <p>Group directories for a conversation and choose where each workspace starts.</p>
        </div>
        <Button disabled={saving || editingName !== null} onClick={() => beginEdit()} type="button">
          Add workspace
        </Button>
      </div>

      <form className="provider-form workspace-default-form" onSubmit={event => void saveDefaultWorkspace(event)}>
        <h3>Default workspace</h3>
        <p>New conversations use this directory when no named workspace is selected.</p>
        <label htmlFor="default-workspace-directory">Starting directory</label>
        <Input
          disabled={saving}
          id="default-workspace-directory"
          onChange={event => setStartingDirectory(event.target.value)}
          required
          value={startingDirectory}
        />
        <Button disabled={saving || !startingDirectory.trim()} type="submit">Save starting directory</Button>
      </form>

      {profile.workspaces.length === 0 ? (
        <p className="settings-empty">No named workspaces yet. The profile’s default workspace is still available.</p>
      ) : (
        <div className="workspace-list">
          {profile.workspaces.map(workspace => (
            <article className="workspace-row" key={workspace.name}>
              <div>
                <h3>{workspace.name}</h3>
                <p>{workspace.directories.length} {workspace.directories.length === 1 ? 'directory' : 'directories'}</p>
                <code>{workspace.default_directory}</code>
              </div>
              <div className="provider-actions">
                <Button disabled={saving} onClick={() => beginEdit(workspace)} type="button" variant="outline">Edit</Button>
                <Button disabled={saving} onClick={() => void removeWorkspace(workspace.name)} type="button" variant="ghost">Delete</Button>
              </div>
            </article>
          ))}
        </div>
      )}

      {editingName !== null ? (
        <form className="provider-form workspace-form" onSubmit={event => void saveWorkspace(event)}>
          <h3>{editingName ? 'Edit workspace' : 'Add workspace'}</h3>
          <label htmlFor="workspace-name">Name</label>
          <Input disabled={saving} id="workspace-name" onChange={event => setName(event.target.value)} required value={name} />
          <label htmlFor="workspace-directories">Directories</label>
          <Textarea
            aria-describedby="workspace-directories-help"
            disabled={saving}
            id="workspace-directories"
            onChange={event => {
              setDirectories(event.target.value);
              const next = event.target.value.split('\n').map(value => value.trim()).filter(Boolean);
              if (!next.includes(defaultDirectory)) setDefaultDirectory(next[0] ?? '');
            }}
            required
            rows={5}
            value={directories}
          />
          <p id="workspace-directories-help">One directory path per line on the machine running Rynna.</p>
          <label htmlFor="workspace-default-directory">Default directory</label>
          <select disabled={saving || directoryOptions.length === 0} id="workspace-default-directory" onChange={event => setDefaultDirectory(event.target.value)} required value={defaultDirectory}>
            {directoryOptions.length === 0 ? <option value="">Add a directory first</option> : null}
            {directoryOptions.map(directory => <option key={directory} value={directory}>{directory}</option>)}
          </select>
          <div className="provider-actions">
            <Button disabled={saving} type="submit">Save workspace</Button>
            <Button disabled={saving} onClick={cancelEdit} type="button" variant="ghost">Cancel</Button>
          </div>
        </form>
      ) : null}
      {error ? <p className="request-error" role="alert">{error}</p> : null}
    </div>
  );
}
