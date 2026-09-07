import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import type { AgentClient, Profile, Workflow, WorkflowRun } from '../contracts';
import { WorkflowSettings } from './workflow-settings';
import { WorkflowPanel } from './workflow-panel';
const workflow: Workflow = { id: 'rynna-default', name: 'Rynna default', revision: 1, description: 'Plan and check', steps: [{ id: 'work', role: 'work', executor: 'instructions', instructions: 'Work' }, { id: 'verify', role: 'verify', executor: 'instructions', instructions: 'Verify', repeat_target: 'work' }] };
const profile: Profile = { name: 'work', providers: [], active_skills: [], mcp_servers: [], capabilities: [], default_project_directory: '.', projects: [], subagents: [] };
const metadata = [{ ...workflow, read_only: true }];
it('keeps the built-in read-only and duplicates to an independently editable draft', async () => {
  const client: AgentClient = { respond: vi.fn(), listWorkflows: vi.fn().mockResolvedValue(metadata), readWorkflow: vi.fn().mockResolvedValue(workflow), saveWorkflow: vi.fn().mockImplementation(async (_, w) => w) };
  render(<WorkflowSettings client={client} profile={profile} />);
  await userEvent.click(await screen.findByRole('button', { name: 'View' }));
  expect(screen.getByLabelText('Name')).toBeDisabled();
  await userEvent.click(screen.getByRole('button', { name: 'Duplicate' }));
  await waitFor(() => expect(screen.getByLabelText('Name')).toBeEnabled());
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'My workflow' } });
  await userEvent.click(screen.getByRole('button', { name: 'Save workflow' }));
  expect(client.saveWorkflow).toHaveBeenCalledWith('work', expect.objectContaining({ name: 'My workflow', revision: 1 }));
  expect(workflow.name).toBe('Rynna default');
});
it('requires explicit start and preserves the request ID after a lost response', async () => {
  const client: AgentClient = { respond: vi.fn(), listWorkflows: vi.fn().mockResolvedValue(metadata), listWorkflowRuns: vi.fn().mockResolvedValue([]), startWorkflow: vi.fn().mockRejectedValue(new Error('response lost')) };
  render(<WorkflowPanel client={client} profile="work" session="session" project={null} selection={{ provider: 'fake', model: 'fake', thinking: 'default' }} selected="rynna-default" context="" onSelection={vi.fn()} onRun={vi.fn()} />);
  expect(client.startWorkflow).not.toHaveBeenCalled();
  fireEvent.change(screen.getByLabelText('Goal'), { target: { value: 'Build' } });
  fireEvent.change(screen.getByLabelText('Success criteria · one per line'), { target: { value: 'Tests pass' } });
  await userEvent.click(screen.getByRole('button', { name: 'Start workflow' }));
  await screen.findByText(/response lost/);
  await userEvent.click(screen.getByRole('button', { name: 'Start workflow' }));
  expect(vi.mocked(client.startWorkflow!).mock.calls[0]![0]).toEqual(vi.mocked(client.startWorkflow!).mock.calls[1]![0]);
});
it('restores an uncertain run by session and requires acknowledgement to resume', async () => {
  const run = { id: 'run', status: 'paused', uncertain: true, revision: 4, cursor: 0, workflow, start: { goal: 'Build', criteria: [{ id: 'done', text: 'Done' }], limits: { steps: 50, tool_calls: 512, active_seconds: 1800 } }, consumed: { steps: 1, tool_calls: 64, active_seconds: 300 }, events: [], verification: null } as unknown as WorkflowRun;
  const client: AgentClient = { respond: vi.fn(), listWorkflows: vi.fn().mockResolvedValue(metadata), listWorkflowRuns: vi.fn().mockResolvedValue([run]), controlWorkflow: vi.fn().mockResolvedValue({ ...run, status: 'running' }) };
  render(<WorkflowPanel client={client} profile="work" session="session" project={null} selection={{ provider: 'fake', model: 'fake', thinking: 'default' }} context="" onSelection={vi.fn()} onRun={vi.fn()} />);
  expect(await screen.findByRole('button', { name: 'Resume' })).toBeDisabled();
  await userEvent.click(screen.getByRole('checkbox'));
  await userEvent.click(screen.getByRole('button', { name: 'Resume' }));
  expect(client.controlWorkflow).toHaveBeenCalledWith('run', { profile: 'work', session_id: 'session', expected_revision: 4, action: 'resume', acknowledge_uncertain: true });
});

it.each([
  ['short multilingual context', '你好🙂 café', '你好🙂 café'],
  ['ASCII suffix', 'a'.repeat(17000), 'a'.repeat(16000)],
  ['CJK boundary', '界'.repeat(6000), '界'.repeat(5333)],
  ['emoji boundary', '🙂'.repeat(5000) + 'x', '🙂'.repeat(3999) + 'x'],
])('starts with a valid UTF-8 context suffix: %s', async (_, context, expected) => {
  const client: AgentClient = { respond: vi.fn(), listWorkflows: vi.fn().mockResolvedValue(metadata), listWorkflowRuns: vi.fn().mockResolvedValue([]), startWorkflow: vi.fn().mockRejectedValue(new Error('response lost')) };
  render(<WorkflowPanel client={client} profile="work" session="session" project={null} selection={{ provider: 'fake', model: 'fake', thinking: 'default' }} selected="rynna-default" context={context} onSelection={vi.fn()} onRun={vi.fn()} />);
  fireEvent.change(screen.getByLabelText('Goal'), { target: { value: 'Build' } });
  fireEvent.change(screen.getByLabelText('Success criteria · one per line'), { target: { value: 'Tests pass' } });
  await userEvent.click(screen.getByRole('button', { name: 'Start workflow' }));
  const sent = vi.mocked(client.startWorkflow!).mock.calls[0]![0].initial_context;
  expect(sent).toBe(expected);
  expect(new TextEncoder().encode(sent).length).toBeLessThanOrEqual(16000);
});

it.each(['running', 'pausing'] as const)('offers Stop for a %s workflow and waits for host settlement', async (status) => {
  const run = { id: 'run', status, revision: 4, cursor: 0, workflow, start: { goal: 'Build', criteria: [], limits: { steps: 50, tool_calls: 512, active_seconds: 1800 } }, consumed: { steps: 1, tool_calls: 64, active_seconds: 300 }, events: [] } as unknown as WorkflowRun;
  const client: AgentClient = { respond: vi.fn(), listWorkflows: vi.fn().mockResolvedValue(metadata), listWorkflowRuns: vi.fn().mockResolvedValue([run]), controlWorkflow: vi.fn().mockResolvedValue({ ...run, status: 'cancelling', revision: 5 }) };
  render(<WorkflowPanel client={client} profile="work" session="session" project={null} selection={{ provider: 'fake', model: 'fake', thinking: 'default' }} selected="rynna-default" context="" onSelection={vi.fn()} onRun={vi.fn()} />);
  await userEvent.click(await screen.findByRole('button', { name: 'Stop' }));
  expect(client.controlWorkflow).toHaveBeenCalledWith('run', { profile: 'work', session_id: 'session', expected_revision: 4, action: 'cancel' });
  expect(await screen.findByRole('button', { name: 'Stopping…' })).toBeDisabled();
});
