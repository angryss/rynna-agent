import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { ToolCallList } from './tool-call-list';
import type { ToolCallActivity } from '../sessions';

afterEach(() => vi.useRealTimers());

const call: ToolCallActivity = { id: 'one', name: 'read_file', arguments: { path: 'src/App.tsx' }, started_at: 1000, status: 'running' };

it('ticks running elapsed time and freezes the reported completion duration', () => {
  vi.useFakeTimers();
  vi.setSystemTime(1000);
  const view = render(<ToolCallList calls={[call]} />);
  expect(screen.getByText('(0.0s)')).toBeInTheDocument();
  act(() => vi.advanceTimersByTime(1200));
  expect(screen.getByText('(1.2s)')).toBeInTheDocument();
  view.rerender(<ToolCallList calls={[{ ...call, status: 'completed', elapsed_ms: 1200 }]} />);
  act(() => vi.advanceTimersByTime(5000));
  expect(screen.getByText('(1.2s)')).toBeInTheDocument();
  expect(screen.getByText('Completed')).toBeInTheDocument();
  expect(vi.getTimerCount()).toBe(0);
});

it('bounds previews and keeps full arguments behind a keyboard-accessible disclosure', async () => {
  const user = userEvent.setup();
  const path = 'a'.repeat(150);
  render(<ToolCallList calls={[{ ...call, status: 'completed', elapsed_ms: 100, arguments: { path } }]} />);
  const summary = screen.getByText('Read File').closest('summary')!;
  const details = summary.closest('details')!;
  expect(summary).toHaveTextContent(`${'a'.repeat(99)}…`);
  expect(details).not.toHaveAttribute('open');
  await user.tab();
  expect(summary).toHaveFocus();
  // Native summary activation is browser-provided; jsdom supports its click default action.
  fireEvent.click(summary);
  expect(details).toHaveAttribute('open');
  expect(details.querySelector('pre')).toHaveTextContent(path);
});
