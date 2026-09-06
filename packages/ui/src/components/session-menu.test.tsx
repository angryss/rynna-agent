import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { SessionMenu } from './session-menu';

it('opens from the keyboard, restores focus on Escape, and only deletes when selected', async () => {
  const onDelete = vi.fn();
  const user = userEvent.setup();
  render(<SessionMenu name="Saved chat" disabled={false} onDelete={onDelete} />);
  const trigger = screen.getByRole('button', { name: 'More options for Saved chat' });
  await user.tab();
  expect(trigger).toHaveFocus();
  await user.keyboard('{ArrowDown}');
  expect(screen.getByRole('menuitem', { name: 'Delete' })).toHaveFocus();
  expect(onDelete).not.toHaveBeenCalled();
  await user.keyboard('{Escape}');
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  await user.keyboard('{Enter}');
  await user.keyboard('{Enter}');
  expect(onDelete).toHaveBeenCalledOnce();
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
});

it('dismisses on outside interaction and when the session becomes disabled', async () => {
  const user = userEvent.setup();
  const onDelete = vi.fn();
  const view = render(<><SessionMenu name="Saved chat" disabled={false} onDelete={onDelete} /><button>Outside</button></>);
  await user.click(screen.getByRole('button', { name: 'More options for Saved chat' }));
  await user.click(screen.getByRole('button', { name: 'Outside' }));
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'More options for Saved chat' }));
  view.rerender(<><SessionMenu name="Saved chat" disabled onDelete={onDelete} /><button>Outside</button></>);
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  expect(onDelete).not.toHaveBeenCalled();
});
