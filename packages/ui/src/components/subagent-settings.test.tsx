import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import type { AgentClient, Profile } from '../contracts';
import { SubagentSettings } from './subagent-settings';

const helper = { name: 'reviewer', description: 'Review code', instructions: 'Find bugs' };
const profile: Profile = { name: 'work', providers: [{ provider: 'ollama', model: 'local' }], active_skills: ['skill'], mcp_servers: [], capabilities: [], default_project_directory: '.', projects: [], subagents: [] };

function Harness({ client, initial = profile }: { client: AgentClient; initial?: Profile }) {
  const [current, setCurrent] = useState(initial);
  return <SubagentSettings client={client} profile={current} onSaved={setCurrent} />;
}

describe('Subagent settings', () => {
  it('adds, edits, and deletes helpers only on the selected profile', async () => {
    const updateProfile = vi.fn().mockImplementation(async (_name, next) => next);
    const user = userEvent.setup();
    render(<Harness client={{ respond: vi.fn(), updateProfile }} />);
    await user.click(screen.getByRole('button', { name: 'Add subagent' }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: helper.name } });
    fireEvent.change(screen.getByLabelText('When to use this helper'), { target: { value: helper.description } });
    fireEvent.change(screen.getByLabelText('Instructions'), { target: { value: helper.instructions } });
    await user.click(screen.getByRole('button', { name: 'Save subagent' }));
    expect(updateProfile).toHaveBeenLastCalledWith('work', { ...profile, subagents: [helper] });
    expect(await screen.findByRole('status')).toHaveTextContent('saved for work');
    await user.click(screen.getByRole('button', { name: 'Edit reviewer' }));
    fireEvent.change(screen.getByLabelText('Instructions'), { target: { value: 'Check tests too' } });
    await user.click(screen.getByRole('button', { name: 'Save subagent' }));
    expect(updateProfile).toHaveBeenLastCalledWith('work', { ...profile, subagents: [{ ...helper, instructions: 'Check tests too' }] });
    await user.click(screen.getByRole('button', { name: 'Delete reviewer' }));
    expect(updateProfile).toHaveBeenLastCalledWith('work', profile);
    expect(await screen.findByText(/No subagents in this profile/)).toBeInTheDocument();
  });

  it('rejects duplicate names and preserves the draft when persistence fails', async () => {
    const updateProfile = vi.fn().mockRejectedValue(new Error('Disk write failed'));
    const user = userEvent.setup();
    render(<Harness client={{ respond: vi.fn(), updateProfile }} initial={{ ...profile, subagents: [helper] }} />);
    await user.click(screen.getByRole('button', { name: 'Add subagent' }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'reviewer' } });
    fireEvent.change(screen.getByLabelText('When to use this helper'), { target: { value: 'Review' } });
    fireEvent.change(screen.getByLabelText('Instructions'), { target: { value: 'Find bugs' } });
    await user.click(screen.getByRole('button', { name: 'Save subagent' }));
    expect(screen.getByRole('alert')).toHaveTextContent('already exists');
    expect(updateProfile).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'writer' } });
    await user.click(screen.getByRole('button', { name: 'Save subagent' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Disk write failed');
    expect(screen.getByLabelText('Name')).toHaveValue('writer');
    expect(screen.getByRole('button', { name: 'Edit reviewer' })).toBeInTheDocument();
  });

  it('clears drafts on profile switching and keeps a late save scoped to its original profile', async () => {
    let finish!: (profile: Profile) => void;
    const updateProfile = vi.fn().mockImplementation(() => new Promise<Profile>(resolve => { finish = resolve; }));
    const onSaved = vi.fn();
    const client = { respond: vi.fn(), updateProfile };
    const user = userEvent.setup();
    const view = render(<SubagentSettings key="work" client={client} profile={{ ...profile, subagents: [helper] }} onSaved={onSaved} />);
    await user.click(screen.getByRole('button', { name: 'Edit reviewer' }));
    await user.click(screen.getByRole('button', { name: 'Save subagent' }));
    view.rerender(<SubagentSettings key="personal" client={client} profile={{ ...profile, name: 'personal' }} onSaved={onSaved} />);
    finish({ ...profile, subagents: [helper] });
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith({ ...profile, subagents: [helper] }));
    expect(screen.queryByLabelText('Name')).not.toBeInTheDocument();
    expect(screen.getByText(/No subagents in this profile/)).toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });
});
