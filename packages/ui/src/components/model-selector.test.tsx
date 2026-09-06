import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { ModelSelector } from './model-selector';

const profile = { name: 'work', providers: [
  { provider: 'local', model: 'small', default: true },
  { provider: 'cloud', model: 'deep' },
  { provider: 'cloud', model: 'hidden', enabled: false },
], active_skills: [], mcp_servers: [], capabilities: [], default_project_directory: '.', projects: [], subagents: [] };

it('searches provider groups and chooses a model with the keyboard without submitting', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  const onSubmit = vi.fn(event => event.preventDefault());
  render(<form onSubmit={onSubmit}><ModelSelector profile={profile} disabled={false} onChange={onChange} /></form>);
  const trigger = screen.getByRole('button', { name: /^Choose model:/ });
  await user.click(trigger);
  expect(screen.getByRole('searchbox', { name: 'Search models' })).toHaveFocus();
  expect(screen.getByRole('region', { name: 'cloud' })).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'hidden' })).not.toBeInTheDocument();
  await user.type(screen.getByRole('searchbox'), 'cloud');
  expect(screen.queryByRole('region', { name: 'local' })).not.toBeInTheDocument();
  await user.keyboard('{ArrowDown}{Enter}');
  expect(onChange).toHaveBeenCalledWith({ provider: 'cloud', model: 'deep', thinking: 'default' });
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  expect(onSubmit).not.toHaveBeenCalled();
});

it('supports empty search, Escape, outside dismissal, and default-model thinking', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<><ModelSelector profile={profile} disabled={false} onChange={onChange} /><button>Outside</button></>);
  const trigger = screen.getByRole('button', { name: /^Choose model:/ });
  await user.click(trigger);
  await user.type(screen.getByRole('searchbox'), 'missing');
  expect(screen.getByText('No models match “missing”.')).toBeInTheDocument();
  await user.keyboard('{Enter}{Escape}');
  expect(onChange).not.toHaveBeenCalled();
  expect(trigger).toHaveFocus();
  await user.click(trigger);
  await user.click(screen.getByRole('radio', { name: 'Medium' }));
  expect(onChange).toHaveBeenCalledWith({ provider: 'local', model: 'small', thinking: 'medium' });
  await user.click(screen.getByRole('button', { name: 'Outside' }));
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});

it('marks the chosen model and closes an open picker when a response starts', async () => {
  const user = userEvent.setup();
  const props = { profile, selection: { provider: 'cloud', model: 'deep', thinking: 'high' as const }, onChange: vi.fn() };
  const { rerender } = render(<ModelSelector {...props} disabled={false} />);
  await user.click(screen.getByRole('button', { name: 'Choose model: deep · High' }));
  expect(within(screen.getByRole('dialog')).getByRole('button', { name: 'deep' })).toHaveAttribute('aria-pressed', 'true');
  rerender(<ModelSelector {...props} disabled />);
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: /^Choose model:/ })).toBeDisabled();
});
