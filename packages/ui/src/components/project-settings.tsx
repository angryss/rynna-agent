import { FormEvent, useEffect, useState } from 'react';

import type { AgentClient, Profile, Project } from '../contracts';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';

interface ProjectSettingsProps {
  client: AgentClient;
  profile: Profile;
  onSaved(profile: Profile): void;
}

export function ProjectSettings({ client, profile, onSaved }: ProjectSettingsProps) {
  const [startingDirectory, setStartingDirectory] = useState(profile.default_project_directory);
  const [editingName, setEditingName] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [directories, setDirectories] = useState('');
  const [defaultDirectory, setDefaultDirectory] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setStartingDirectory(profile.default_project_directory);
    cancelEdit();
  }, [profile.name, profile.default_project_directory]);

  async function save(next: Profile): Promise<boolean> {
    if (!client.updateProfile || saving) return false;
    setSaving(true);
    setError(null);
    try {
      onSaved(await client.updateProfile(profile.name, next));
      return true;
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : 'Rynna could not save project settings');
      return false;
    } finally {
      setSaving(false);
    }
  }

  function beginEdit(project?: Project) {
    setEditingName(project?.name ?? '');
    setName(project?.name ?? '');
    setDirectories(project?.directories.join('\n') ?? '');
    setDefaultDirectory(project?.default_directory ?? '');
    setError(null);
  }

  function cancelEdit() {
    setEditingName(null);
    setName('');
    setDirectories('');
    setDefaultDirectory('');
    setError(null);
  }

  async function saveDefaultProject(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = startingDirectory.trim();
    if (!value) return;
    await save({ ...profile, default_project_directory: value });
  }

  async function saveProject(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const nextDirectories = directories.split('\n').map(value => value.trim()).filter(Boolean);
    const project: Project = {
      name: name.trim(),
      directories: nextDirectories,
      default_directory: defaultDirectory.trim(),
    };
    if (!project.name || nextDirectories.length === 0 || !nextDirectories.includes(project.default_directory)) {
      setError('Enter a name and choose a default directory from the project directories.');
      return;
    }
    const projects = editingName
      ? profile.projects.map(candidate => candidate.name === editingName ? project : candidate)
      : [...profile.projects, project];
    if (await save({ ...profile, projects })) cancelEdit();
  }

  async function removeProject(projectName: string) {
    const removed = await save({
      ...profile,
      projects: profile.projects.filter(project => project.name !== projectName),
    });
    if (removed && editingName === projectName) cancelEdit();
  }

  const directoryOptions = directories.split('\n').map(value => value.trim()).filter(Boolean);

  return (
    <div className="project-settings">
      <div className="settings-heading">
        <div>
          <h2>Projects</h2>
          <p>Group directories for a conversation and choose where each project starts.</p>
        </div>
        <Button disabled={saving || editingName !== null} onClick={() => beginEdit()} type="button">
          Add project
        </Button>
      </div>

      <form className="provider-form project-default-form" onSubmit={event => void saveDefaultProject(event)}>
        <h3>Default project</h3>
        <p>New conversations use this directory when no named project is selected.</p>
        <label htmlFor="default-project-directory">Starting directory</label>
        <Input
          disabled={saving}
          id="default-project-directory"
          onChange={event => setStartingDirectory(event.target.value)}
          required
          value={startingDirectory}
        />
        <Button disabled={saving || !startingDirectory.trim()} type="submit">Save starting directory</Button>
      </form>

      {profile.projects.length === 0 ? (
        <p className="settings-empty">No named projects yet. The profile’s default project is still available.</p>
      ) : (
        <div className="project-list">
          {profile.projects.map(project => (
            <article className="project-row" key={project.name}>
              <div>
                <h3>{project.name}</h3>
                <p>{project.directories.length} {project.directories.length === 1 ? 'directory' : 'directories'}</p>
                <code>{project.default_directory}</code>
              </div>
              <div className="provider-actions">
                <Button disabled={saving} onClick={() => beginEdit(project)} type="button" variant="outline">Edit</Button>
                <Button disabled={saving} onClick={() => void removeProject(project.name)} type="button" variant="ghost">Delete</Button>
              </div>
            </article>
          ))}
        </div>
      )}

      {editingName !== null ? (
        <form className="provider-form project-form" onSubmit={event => void saveProject(event)}>
          <h3>{editingName ? 'Edit project' : 'Add project'}</h3>
          <label htmlFor="project-name">Name</label>
          <Input disabled={saving} id="project-name" onChange={event => setName(event.target.value)} required value={name} />
          <label htmlFor="project-directories">Directories</label>
          <Textarea
            aria-describedby="project-directories-help"
            disabled={saving}
            id="project-directories"
            onChange={event => {
              setDirectories(event.target.value);
              const next = event.target.value.split('\n').map(value => value.trim()).filter(Boolean);
              if (!next.includes(defaultDirectory)) setDefaultDirectory(next[0] ?? '');
            }}
            required
            rows={5}
            value={directories}
          />
          <p id="project-directories-help">One directory path per line on the machine running Rynna.</p>
          <label htmlFor="project-default-directory">Default directory</label>
          <select disabled={saving || directoryOptions.length === 0} id="project-default-directory" onChange={event => setDefaultDirectory(event.target.value)} required value={defaultDirectory}>
            {directoryOptions.length === 0 ? <option value="">Add a directory first</option> : null}
            {directoryOptions.map(directory => <option key={directory} value={directory}>{directory}</option>)}
          </select>
          <div className="provider-actions">
            <Button disabled={saving} type="submit">Save project</Button>
            <Button disabled={saving} onClick={cancelEdit} type="button" variant="ghost">Cancel</Button>
          </div>
        </form>
      ) : null}
      {error ? <p className="request-error" role="alert">{error}</p> : null}
    </div>
  );
}
