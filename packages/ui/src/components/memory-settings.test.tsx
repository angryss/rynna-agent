import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../App';
import { MemorySettingsPanel } from './memory-settings';

const cloud = {
  kind: 'hindsight' as const,
  deployment: 'cloud' as const,
  api_base: 'https://api.hindsight.vectorize.io',
  bank_id: 'rynna',
  api_key_configured: true,
};

function catalog() {
  const profiles = ['test', 'other'].map((name) => ({
    name,
    providers: [{ provider: 'ollama', model: 'model' }],
    active_skills: [],
    mcp_servers: [],
    capabilities: [],
    default_project_directory: '.',
    projects: [],
  }));
  return {
    default_profile: 'test',
    provider_ids: ['ollama'],
    profiles,
    configured_profiles: profiles,
  };
}

describe('Memory settings', () => {
  it('opens from Settings with None selected and reveals only the selected provider fields', async () => {
    const user = userEvent.setup();
    const save = vi.fn().mockResolvedValue({
      ...cloud,
      deployment: 'self_hosted',
      api_base: 'http://localhost:8888',
      api_key_configured: false,
    });
    render(
      <App
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue(catalog()),
          getMemorySettings: vi.fn().mockResolvedValue({ kind: 'none' }),
          saveMemorySettings: save,
        }}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    const provider = await screen.findByLabelText('Memory provider');
    expect(provider).toHaveValue('none');
    expect(screen.queryByLabelText('API URL')).not.toBeInTheDocument();
    await user.selectOptions(provider, 'hindsight');
    expect(screen.getByLabelText('Hosting')).toHaveValue('cloud');
    expect(screen.getByLabelText('API URL')).toHaveValue(cloud.api_base);
    expect(screen.getByLabelText('API URL')).toHaveAttribute('readonly');
    expect(screen.getByLabelText('API key')).toBeRequired();
    await user.selectOptions(screen.getByLabelText('Hosting'), 'self_hosted');
    expect(screen.getByLabelText('API URL')).toHaveValue(
      'http://localhost:8888',
    );
    expect(screen.getByLabelText('API key (optional)')).not.toBeRequired();
    await user.type(screen.getByLabelText('Memory bank ID'), 'rynna');
    await user.click(
      screen.getByRole('button', { name: 'Save memory settings' }),
    );
    expect(save).toHaveBeenCalledWith(
      {
        kind: 'hindsight',
        deployment: 'self_hosted',
        api_base: 'http://localhost:8888',
        bank_id: 'rynna',
      },
      'test',
    );
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Changes apply to your next request',
    );
    await user.selectOptions(provider, 'none');
    expect(screen.queryByLabelText('Memory bank ID')).not.toBeInTheDocument();
    await user.click(
      screen.getByRole('button', { name: 'Save memory settings' }),
    );
    expect(save).toHaveBeenLastCalledWith({ kind: 'none' }, 'test');
  });

  it('keeps existing keys on blank saves and clears entered keys after saving', async () => {
    const user = userEvent.setup();
    const save = vi.fn().mockResolvedValue(cloud);
    render(
      <MemorySettingsPanel
        profile="test"
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue(catalog()),
          getMemorySettings: vi.fn().mockResolvedValue(cloud),
          saveMemorySettings: save,
        }}
      />,
    );
    await screen.findByText('A key is saved. Leave this blank to keep it.');
    expect(screen.getByLabelText('API key')).toHaveValue('');
    await user.click(
      screen.getByRole('button', { name: 'Save memory settings' }),
    );
    expect(save).toHaveBeenLastCalledWith(
      {
        kind: 'hindsight',
        deployment: 'cloud',
        api_base: cloud.api_base,
        bank_id: 'rynna',
      },
      'test',
    );
    await user.type(screen.getByLabelText('API key'), 'replacement-test-key');
    await user.click(
      screen.getByRole('button', { name: 'Save memory settings' }),
    );
    expect(save).toHaveBeenLastCalledWith(
      expect.objectContaining({ api_key: 'replacement-test-key' }),
      'test',
    );
    expect(screen.getByLabelText('API key')).toHaveValue('');
  });

  it('reports failed saves without claiming success', async () => {
    const user = userEvent.setup();
    render(
      <MemorySettingsPanel
        profile="test"
        client={{
          respond: vi.fn(),
          listProfiles: vi.fn().mockResolvedValue(catalog()),
          getMemorySettings: vi.fn().mockResolvedValue(cloud),
          saveMemorySettings: vi
            .fn()
            .mockRejectedValue(new Error('Could not save memory settings')),
        }}
      />,
    );
    await screen.findByText('A key is saved. Leave this blank to keep it.');
    await user.click(
      screen.getByRole('button', { name: 'Save memory settings' }),
    );
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Could not save memory settings',
    );
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });
});

it('switches profiles without copying drafts, credentials, or saved status', async () => {
  const user = userEvent.setup();
  const load = vi
    .fn()
    .mockImplementation(async (profile: string) =>
      profile === 'test' ? cloud : { kind: 'none' },
    );
  const save = vi.fn().mockResolvedValue({ kind: 'none' });
  render(
    <App
      client={{
        respond: vi.fn(),
        listProfiles: vi.fn().mockResolvedValue(catalog()),
        getMemorySettings: load,
        saveMemorySettings: save,
      }}
    />,
  );
  await user.click(screen.getByRole('button', { name: 'Settings' }));
  await user.click(screen.getByRole('button', { name: 'Memory provider' }));
  await screen.findByText('A key is saved. Leave this blank to keep it.');
  await user.type(screen.getByLabelText('API key'), 'unsaved-secret');
  await user.selectOptions(screen.getByLabelText('Profile'), 'other');
  expect(await screen.findByLabelText('Memory provider')).toHaveValue('none');
  expect(load).toHaveBeenLastCalledWith('other');
  await user.selectOptions(
    screen.getByLabelText('Memory provider'),
    'hindsight',
  );
  expect(screen.getByLabelText('API key')).toHaveValue('');
  expect(screen.getByLabelText('API key')).toBeRequired();
  expect(screen.getByLabelText('Memory bank ID')).toHaveValue('');
  await user.selectOptions(screen.getByLabelText('Memory provider'), 'none');
  await user.click(
    screen.getByRole('button', { name: 'Save memory settings' }),
  );
  expect(save).toHaveBeenLastCalledWith({ kind: 'none' }, 'other');
  await user.selectOptions(screen.getByLabelText('Profile'), 'test');
  await screen.findByText('A key is saved. Leave this blank to keep it.');
  expect(screen.getByLabelText('Memory bank ID')).toHaveValue('rynna');
  expect(screen.getByLabelText('API key')).toHaveValue('');
});

it('ignores a previous profile load that resolves after switching', async () => {
  const user = userEvent.setup();
  let finishFirst!: (value: typeof cloud) => void;
  const first = new Promise<typeof cloud>((resolve) => {
    finishFirst = resolve;
  });
  const load = vi
    .fn()
    .mockImplementation((profile: string) =>
      profile === 'test' ? first : Promise.resolve({ kind: 'none' }),
    );
  render(
    <App
      client={{
        respond: vi.fn(),
        listProfiles: vi.fn().mockResolvedValue(catalog()),
        getMemorySettings: load,
        saveMemorySettings: vi.fn(),
      }}
    />,
  );
  await user.click(screen.getByRole('button', { name: 'Settings' }));
  await user.click(screen.getByRole('button', { name: 'Memory provider' }));
  await user.selectOptions(screen.getByLabelText('Profile'), 'other');
  await act(async () => {
    finishFirst(cloud);
    await first;
  });
  expect(screen.getByLabelText('Memory provider')).toHaveValue('none');
  expect(screen.queryByLabelText('API key')).not.toBeInTheDocument();
});

it.each(['none', 'hindsight'])('allows saving %s after a failed load', async (kind) => {
  const user = userEvent.setup();
  const save = vi.fn().mockResolvedValue({ kind: 'none' });
  render(
    <MemorySettingsPanel profile="test" client={{
      respond: vi.fn(),
      getMemorySettings: vi.fn().mockRejectedValue(new Error('Invalid profile settings')),
      saveMemorySettings: save,
    }} />,
  );
  await screen.findByRole('alert');
  expect(screen.getByLabelText('Memory provider')).toBeEnabled();
  if (kind === 'hindsight') {
    await user.selectOptions(screen.getByLabelText('Memory provider'), 'hindsight');
    await user.selectOptions(screen.getByLabelText('Hosting'), 'self_hosted');
    await user.type(screen.getByLabelText('Memory bank ID'), 'repaired');
  }
  await user.click(screen.getByRole('button', { name: 'Save memory settings' }));
  expect(save).toHaveBeenCalledWith(expect.objectContaining({ kind }), 'test');
  expect(await screen.findByRole('status')).toHaveTextContent('Memory settings saved');
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Retry loading memory settings' })).not.toBeInTheDocument();
});
