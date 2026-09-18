import { render, screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import type { AgentClient, Profile } from '../contracts';
import { ToolsetSettings } from './toolset-settings';

const profile: Profile = { name: 'work', providers: [], active_skills: [], mcp_servers: [], capabilities: [], default_project_directory: '.', projects: [], subagents: [], disabled_toolsets: ['commands'] };
function Harness({ client }: { client: AgentClient }) {
  const [current, setCurrent] = useState(profile);
  return <ToolsetSettings client={client} profile={current} onSaved={setCurrent} />;
}

describe('Toolset settings', () => {
  it('reenables one group without resetting the other switches', async () => {
    const user = userEvent.setup();
    const updateProfile = vi.fn().mockImplementation(async (_name, next) => next);
    render(<Harness client={{ respond: vi.fn(), updateProfile }} />);
    const files = screen.getByRole('article', { name: 'File Operations' });
    await user.click(within(files).getByRole('button'));
    await user.click(screen.getByRole('checkbox', { name: 'Enable File Operations' }));
    await user.click(screen.getByRole('button', { name: 'Save toolset' }));
    expect(updateProfile).toHaveBeenLastCalledWith('work', { ...profile, disabled_toolsets: ['commands', 'file_operations'] });
    await user.click(within(files).getByRole('button'));
    await user.click(screen.getByRole('checkbox', { name: 'Enable File Operations' }));
    await user.click(screen.getByRole('button', { name: 'Save toolset' }));
    expect(updateProfile).toHaveBeenLastCalledWith('work', profile);
  });

  it('keeps the saved status and editable draft when persistence fails', async () => {
    const user = userEvent.setup();
    const updateProfile = vi.fn().mockRejectedValue(new Error('Disk full'));
    render(<Harness client={{ respond: vi.fn(), updateProfile }} />);
    await user.click(screen.getByRole('button', { name: 'Configure Commands' }));
    await user.click(screen.getByRole('checkbox', { name: 'Enable Commands' }));
    await user.click(screen.getByRole('button', { name: 'Save toolset' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Disk full');
    expect(within(screen.getByRole('article', { name: 'Commands' })).getByText('Disabled')).toBeInTheDocument();
    expect(screen.getByRole('checkbox')).toBeChecked();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it('keeps late saves scoped to their original profile', async () => {
    const user = userEvent.setup();
    let finish!: (profile: Profile) => void;
    const updateProfile = vi.fn().mockImplementation(() => new Promise<Profile>(resolve => { finish = resolve; }));
    const onSaved = vi.fn();
    const client = { respond: vi.fn(), updateProfile };
    const view = render(<ToolsetSettings key="work" client={client} profile={profile} onSaved={onSaved} />);
    await user.click(screen.getByRole('button', { name: 'Configure Commands' }));
    await user.click(screen.getByRole('checkbox'));
    await user.click(screen.getByRole('button', { name: 'Save toolset' }));
    expect(screen.getByRole('button', { name: 'Saving…' })).toBeDisabled();
    view.rerender(<ToolsetSettings key="personal" client={client} profile={{ ...profile, name: 'personal', disabled_toolsets: [] }} onSaved={onSaved} />);
    finish({ ...profile, disabled_toolsets: [] });
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith({ ...profile, disabled_toolsets: [] }));
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });
});
