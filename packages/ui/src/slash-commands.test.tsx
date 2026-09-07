import { act, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import { App } from './App';
import type { AgentClient } from './contracts';

beforeEach(() => window.localStorage.clear());

function setup(extra: Partial<AgentClient> = {}) {
  const respond = vi.fn().mockResolvedValue({ message: { role: 'assistant', content: 'Answer' } });
  render(<App client={{ respond, ...extra }} />);
  return { user: userEvent.setup(), respond, input: screen.getByLabelText('Message Rynna') };
}

it('filters commands, completes with Tab, and executes with Enter without a model call', async () => {
  const { user, input, respond } = setup();
  await user.type(input, '/');
  expect(screen.getAllByRole('option')).toHaveLength(9);
  await user.keyboard('{ArrowDown}{Tab}');
  expect(input).toHaveValue('/clear ');
  expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  await user.keyboard('{Enter}');
  expect(input).toHaveValue('');
  await user.type(input, '/ne');
  expect(screen.getAllByRole('option')).toHaveLength(1);
  await user.keyboard('{Enter}');
  expect(input).toHaveValue('');
  expect(respond).not.toHaveBeenCalled();
});

it('dismisses without changing the draft, preserves modified Enter and IME input, and reopens on editing', async () => {
  const { user, input, respond } = setup();
  await user.type(input, '/');
  await user.keyboard('{Escape}');
  expect(input).toHaveValue('/');
  expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  await user.type(input, 'n');
  expect(screen.getByRole('listbox')).toBeInTheDocument();
  fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
  expect(input).toHaveValue('/n');
  await user.keyboard('{Shift>}{Enter}{/Shift}');
  expect(input).toHaveValue('/n\n');
  expect(respond).not.toHaveBeenCalled();
});

it('rejects unknown commands and invalid arguments instead of sending them', async () => {
  const { user, input, respond } = setup();
  await user.type(input, '/unknown');
  await user.keyboard('{Enter}');
  expect(screen.getByRole('alert')).toHaveTextContent('Unknown command');
  await user.clear(input);
  await user.type(input, '/new extra');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(screen.getByRole('alert')).toHaveTextContent('does not take arguments');
  expect(respond).not.toHaveBeenCalled();
});

it('shows help by mouse and accepts title arguments without executing prematurely', async () => {
  const { user, input, respond } = setup();
  await user.type(input, '/');
  await user.click(screen.getByRole('option', { name: /\/help/ }));
  expect(screen.getByRole('listbox')).toBeInTheDocument();
  await user.click(screen.getByRole('option', { name: /\/title/ }));
  expect(input).toHaveValue('/title ');
  expect(respond).not.toHaveBeenCalled();
});

it('renames a saved chat, retries with only the preceding history, and starts a fresh session', async () => {
  const { user, input, respond } = setup();
  await user.type(input, 'First{Enter}');
  await screen.findByText('Answer');
  await user.type(input, '/title My chat{Enter}');
  expect(screen.getByRole('button', { name: 'My chat' })).toBeInTheDocument();
  await user.type(input, '/retry{Enter}');
  expect(respond).toHaveBeenCalledTimes(2);
  expect(respond.mock.calls[1]![0]).toMatchObject({ prompt: 'First', history: [], session_id: respond.mock.calls[0]![0].session_id });
  expect(within(screen.getByRole('log')).getAllByText('First')).toHaveLength(1);
  await user.type(input, '/new{Enter}');
  expect(within(screen.getByRole('log')).queryByText('First')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'My chat' })).toBeInTheDocument();
  await user.type(input, 'Second{Enter}');
  expect(respond.mock.calls[2]![0].session_id).not.toBe(respond.mock.calls[0]![0].session_id);
});

it('opens the existing model picker and settings locally', async () => {
  const { user, input, respond } = setup({
    listProfiles: vi.fn().mockResolvedValue({ default_profile: 'work', provider_ids: ['ollama'], profiles: [{ name: 'work', providers: [{ provider: 'ollama', model: 'test' }], active_skills: [], mcp_servers: [], capabilities: [], projects: [], subagents: [] }] }),
    listProviders: vi.fn().mockResolvedValue([]),
  });
  await screen.findByRole('button', { name: /Choose model/ });
  await user.type(input, '/model{Enter}');
  expect(screen.getByRole('dialog', { name: 'Choose provider and model' })).toBeInTheDocument();
  await user.keyboard('{Escape}');
  await user.type(input, '/settings{Enter}');
  expect(screen.getByRole('region', { name: 'Settings' })).toBeInTheDocument();
  expect(respond).not.toHaveBeenCalled();
});

it('does not run commands while a response is pending', async () => {
  let finish!: (value: unknown) => void;
  const { user, input } = setup({ respond: vi.fn().mockImplementation(() => new Promise(resolve => { finish = resolve; })) });
  await user.type(input, 'Wait{Enter}');
  await user.type(input, '/new{Enter}');
  expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  expect(input).toHaveValue('/new');
  expect(within(screen.getByRole('log')).getByText('Wait')).toBeInTheDocument();
  await act(async () => finish({ message: { role: 'assistant', content: 'Done' } }));
});

it('exports visible transcript messages as JSON', async () => {
  const create = vi.fn().mockReturnValue('blob:transcript');
  const revoke = vi.fn();
  vi.stubGlobal('URL', Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke }));
  const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
  try {
    const { user, input, respond } = setup();
    await user.type(input, 'Export this{Enter}');
    await screen.findByText('Answer');
    await user.type(input, '/save{Enter}');
    expect(create).toHaveBeenCalledWith(expect.any(Blob));
    const blob = create.mock.calls[0]![0] as Blob;
    const text = await new Promise<string>(resolve => { const reader = new FileReader(); reader.onload = () => resolve(reader.result as string); reader.readAsText(blob); });
    expect(JSON.parse(text).messages).toEqual([{ role: 'user', content: 'Export this' }, { role: 'assistant', content: 'Answer' }]);
    expect(click).toHaveBeenCalledOnce();
    expect(respond).toHaveBeenCalledOnce();
  } finally { click.mockRestore(); vi.unstubAllGlobals(); }
});

it('restores the previous exchange when retry fails', async () => {
  const { user, input, respond } = setup();
  await user.type(input, 'Keep this{Enter}');
  await screen.findByText('Answer');
  respond.mockRejectedValueOnce(new Error('Provider offline'));
  await user.type(input, '/retry{Enter}');
  expect(screen.getByRole('alert')).toHaveTextContent('Provider offline');
  expect(within(screen.getByRole('log')).getByText('Answer')).toBeInTheDocument();
  expect(within(screen.getByRole('log')).getAllByText('Keep this')).toHaveLength(1);
  expect(input).toHaveValue('Keep this');
});

it('compacts without changing the visible transcript and saves the summary for the next turn', async () => {
  const conversationContext = vi.fn().mockImplementation(async request => ({
    history: request.compact ? request.history.map((message: object, index: number) => index === request.history.length - 1
      ? { ...message, provider_context: { provider: 'conversation_summary', state: 'Remember first request' } } : message) : request.history,
    size: { current_tokens: 100, max_tokens: 1000 }, compacted: !!request.compact, limit_known: true,
  }));
  const { user, input, respond } = setup({ conversationContext });
  await user.type(input, 'First{Enter}');
  await screen.findByText('Answer');
  await user.type(input, '/compact{Enter}');
  await screen.findByText('Context compacted. Your transcript is preserved.');
  expect(within(screen.getByRole('log')).getByText('First')).toBeInTheDocument();
  expect(within(screen.getByRole('log')).getByText('Answer')).toBeInTheDocument();
  expect(respond).toHaveBeenCalledTimes(1);
  expect(JSON.parse(window.localStorage.getItem('rynna-sessions-v1')!)[0].messages[1].provider_context.state).toBe('Remember first request');
  await user.type(input, 'Continue{Enter}');
  expect(respond.mock.calls[1]![0].history[1].provider_context.state).toBe('Remember first request');
});

it('preserves history when manual compaction fails', async () => {
  const { user, input, respond } = setup({ conversationContext: vi.fn().mockImplementation(async request => {
    if (request.compact) throw new Error('Summary unavailable');
    return { history: request.history, size: { current_tokens: 1, max_tokens: 1000 }, compacted: false, limit_known: false };
  }) });
  await user.type(input, 'First{Enter}');
  await screen.findByText('Answer');
  await user.type(input, '/compact{Enter}');
  expect(await screen.findByRole('alert')).toHaveTextContent('Summary unavailable');
  expect(within(screen.getByRole('log')).getByText('First')).toBeInTheDocument();
  expect(respond).toHaveBeenCalledTimes(1);
  expect(screen.getByRole('button', { name: 'Send' })).toBeInTheDocument();
});

it('labels the context percentage and unknown-model fallback honestly', async () => {
  setup({ conversationContext: vi.fn().mockResolvedValue({ history: [], size: { current_tokens: 2048, max_tokens: 8192 }, compacted: false, limit_known: false }) });
  const indicator = await screen.findByText('~25% budget');
  expect(indicator).toHaveAttribute('title', expect.stringContaining('Fallback budget'));
});

it('ignores a late compaction failure after the session is deleted in another window', async () => {
  let fail!: (error: Error) => void;
  const { user, input } = setup({ conversationContext: vi.fn().mockImplementation(request => request.compact
    ? new Promise((_resolve, reject) => { fail = reject; })
    : Promise.resolve({ history: request.history, size: { current_tokens: 1, max_tokens: 8192 }, compacted: false, limit_known: false })) });
  await user.type(input, 'First{Enter}');
  await screen.findByText('Answer');
  const id = JSON.parse(window.localStorage.getItem('rynna-sessions-v1')!)[0].id;
  await user.type(input, '/compact{Enter}');
  await screen.findByRole('button', { name: 'Compacting…' });
  await act(async () => {
    const key = `rynna-deleted-session-v1:${id}`;
    window.localStorage.setItem(key, 'true');
    window.dispatchEvent(new StorageEvent('storage', { key, newValue: 'true' }));
    fail(new Error('Late compaction failure'));
  });
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  expect(screen.queryByText('Context compacted. Your transcript is preserved.')).not.toBeInTheDocument();
  expect(within(screen.getByRole('log')).queryByText('First')).not.toBeInTheDocument();
});
