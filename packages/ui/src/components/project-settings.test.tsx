import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { expect, it, vi } from 'vitest';

import type { AgentClient, Profile } from '../contracts';
import { ProjectSettings } from './project-settings';

const initialProfile: Profile = {
  name: 'work',
  providers: [{ provider: 'openai', model: 'gpt-5' }],
  active_skills: [],
  mcp_servers: [],
  capabilities: [],
  default_project_directory: '.',
  projects: [],
  subagents: [],
};

it('updates the default directory and creates, edits, and deletes named projects', async () => {
  const updateProfile = vi.fn(async (_name: string, profile: Profile) => profile);
  const client: AgentClient = { respond: vi.fn(), updateProfile };
  function Harness() {
    const [profile, setProfile] = useState(initialProfile);
    return <ProjectSettings client={client} onSaved={setProfile} profile={profile} />;
  }
  const user = userEvent.setup();
  render(<Harness />);

  await user.clear(screen.getByLabelText('Starting directory'));
  await user.type(screen.getByLabelText('Starting directory'), '/projects/home');
  await user.click(screen.getByRole('button', { name: 'Save starting directory' }));
  expect(updateProfile).toHaveBeenLastCalledWith('work', expect.objectContaining({
    default_project_directory: '/projects/home',
  }));

  await user.click(screen.getByRole('button', { name: 'Add project' }));
  await user.type(screen.getByLabelText('Name'), 'rynna');
  await user.type(screen.getByLabelText('Directories'), '/projects/rynna\n/projects/shared');
  await user.selectOptions(screen.getByLabelText('Default directory'), '/projects/shared');
  await user.click(screen.getByRole('button', { name: 'Save project' }));
  expect(screen.getByRole('heading', { name: 'rynna' })).toBeInTheDocument();
  expect(updateProfile).toHaveBeenLastCalledWith('work', expect.objectContaining({
    projects: [{
      name: 'rynna',
      directories: ['/projects/rynna', '/projects/shared'],
      default_directory: '/projects/shared',
    }],
  }));

  await user.click(screen.getByRole('button', { name: 'Edit' }));
  await user.clear(screen.getByLabelText('Name'));
  await user.type(screen.getByLabelText('Name'), 'agent');
  await user.click(screen.getByRole('button', { name: 'Save project' }));
  expect(screen.getByRole('heading', { name: 'agent' })).toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: 'Delete' }));
  expect(screen.queryByRole('heading', { name: 'agent' })).not.toBeInTheDocument();
  expect(screen.getByText(/No named projects yet/)).toBeInTheDocument();
});
