import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { McpSettingsPanel } from './mcp-settings';
import type { AgentClient, McpSettings } from '../contracts';

const config: McpSettings = { mcpServers: { tools: { transport: 'stdio', command: 'work-command', enabled: false } } };
const empty: McpSettings = { mcpServers: {} };
function client(): AgentClient {
  return { respond: vi.fn(), getMcpSettings: vi.fn().mockResolvedValue(empty), saveMcpSettings: vi.fn().mockImplementation(async value => value) };
}

describe('MCP settings', () => {
  it('saves valid server configurations only for the selected profile and supports removal', async () => {
    const api = client(); const user = userEvent.setup();
    render(<McpSettingsPanel client={api} profile="work" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    fireEvent.change(screen.getByLabelText('Server configuration (JSON)'), { target: { value: JSON.stringify(config) } });
    await user.click(screen.getByRole('button', { name: 'Save MCP servers' }));
    expect(api.saveMcpSettings).toHaveBeenCalledWith(config, 'work');
    expect(await screen.findByRole('status')).toHaveTextContent('saved for work');
    fireEvent.change(screen.getByLabelText('Server configuration (JSON)'), { target: { value: JSON.stringify(empty) } });
    await user.click(screen.getByRole('button', { name: 'Save MCP servers' }));
    expect(api.saveMcpSettings).toHaveBeenLastCalledWith(empty, 'work');
  });

  it('preserves an explicitly selected code search plugin through load and save', async () => {
    const selected: McpSettings = { mcpServers: { search: { transport: 'stdio', command: 'search-plugin', code_search_tool: 'search_code' } } };
    const api = client(); const user = userEvent.setup();
    vi.mocked(api.getMcpSettings!).mockResolvedValue(selected);
    render(<McpSettingsPanel client={api} profile="work" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    await user.click(screen.getByRole('button', { name: 'Save MCP servers' }));
    expect(api.saveMcpSettings).toHaveBeenCalledWith(selected, 'work');
  });

  it('rejects malformed JSON and unsupported server fields without saving', async () => {
    const api = client(); const user = userEvent.setup();
    render(<McpSettingsPanel client={api} profile="work" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    for (const value of ['{broken', '{"mcpServers":{"tools":{"transport":"stdio","command":"search","code_search_tool":42}}}', '{"mcpServers":{"tools":{"transport":"sse","url":"https://example.com"}}}']) {
      fireEvent.change(screen.getByLabelText('Server configuration (JSON)'), { target: { value } });
      await user.click(screen.getByRole('button', { name: 'Save MCP servers' }));
      expect(screen.getByRole('alert')).toBeInTheDocument();
    }
    expect(api.saveMcpSettings).not.toHaveBeenCalled();
  });

  it('discards drafts and ignores a late load from another profile', async () => {
    const api = client(); let resolve!: (value: McpSettings) => void;
    vi.mocked(api.getMcpSettings!).mockImplementation(profile => profile === 'work' ? new Promise(done => { resolve = done; }) : Promise.resolve(empty));
    const view = render(<McpSettingsPanel client={api} profile="work" />);
    view.rerender(<McpSettingsPanel client={api} profile="personal" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    await act(async () => resolve(config));
    expect(screen.getByLabelText('Server configuration (JSON)')).toHaveValue(JSON.stringify(empty, null, 2));
    fireEvent.change(screen.getByLabelText('Server configuration (JSON)'), { target: { value: 'unsaved personal draft' } });
    vi.mocked(api.getMcpSettings!).mockResolvedValue(config);
    view.rerender(<McpSettingsPanel client={api} profile="work" />);
    await waitFor(() => expect(screen.getByLabelText('Server configuration (JSON)')).toHaveValue(JSON.stringify(config, null, 2)));
  });

  it('ignores a late save after switching profile', async () => {
    const api = client(); const user = userEvent.setup(); let resolve!: (value: McpSettings) => void;
    vi.mocked(api.saveMcpSettings!).mockReturnValue(new Promise(done => { resolve = done; }));
    const view = render(<McpSettingsPanel client={api} profile="work" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    await user.click(screen.getByRole('button', { name: 'Save MCP servers' }));
    view.rerender(<McpSettingsPanel client={api} profile="personal" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    await act(async () => resolve(config));
    expect(screen.getByLabelText('Server configuration (JSON)')).toHaveValue(JSON.stringify(empty, null, 2));
    expect(screen.queryByText(/saved for work/)).not.toBeInTheDocument();
  });

  it('allows retrying failed loads and keeps failed saves editable', async () => {
    const api = client(); const user = userEvent.setup();
    vi.mocked(api.getMcpSettings!).mockRejectedValueOnce(new Error('unavailable')).mockResolvedValue(empty);
    vi.mocked(api.saveMcpSettings!).mockRejectedValue(new Error('could not save MCP settings'));
    render(<McpSettingsPanel client={api} profile="work" />);
    await user.click(await screen.findByRole('button', { name: 'Retry loading MCP servers' }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save MCP servers' })).toBeEnabled());
    await user.click(screen.getByRole('button', { name: 'Save MCP servers' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('could not save');
    expect(screen.getByLabelText('Server configuration (JSON)')).toBeEnabled();
  });
});
