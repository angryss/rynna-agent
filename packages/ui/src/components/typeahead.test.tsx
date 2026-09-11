import { useState } from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { Typeahead } from './typeahead';

function Form({ submit }: { submit: (value: string) => void }) {
  const [value, setValue] = useState('');
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        submit(value);
      }}
    >
      <label htmlFor="model">Model name</label>
      <Typeahead
        id="model"
        allowCustom
        options={['alpha-small', 'alpha-large', 'beta']}
        value={value}
        onChange={setValue}
      />
      <button type="submit">Add</button>
    </form>
  );
}
it('filters provider models and supports keyboard selection', async () => {
  const user = userEvent.setup();
  const submit = vi.fn();
  render(<Form submit={submit} />);
  await user.type(screen.getByRole('combobox'), 'ALPHA');
  expect(screen.getAllByRole('option')).toHaveLength(2);
  expect(screen.queryByRole('option', { name: 'beta' })).not.toBeInTheDocument();
  await user.keyboard('{ArrowDown}{Enter}');
  expect(screen.getByRole('combobox')).toHaveValue('alpha-large');
  expect(submit).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: 'Add' }));
  expect(submit).toHaveBeenCalledWith('alpha-large');
});
it('preserves unlisted values on blur and submits raw text without auto-selecting suggestions', async () => {
  const user = userEvent.setup();
  const submit = vi.fn();
  render(<Form submit={submit} />);
  await user.type(screen.getByRole('combobox'), 'alpha');
  await user.keyboard('{Enter}');
  expect(submit).toHaveBeenCalledWith('alpha');
  fireEvent.change(screen.getByRole('combobox'), { target: { value: 'private/model:v2' } });
  await user.tab();
  expect(screen.getByRole('combobox')).toHaveValue('private/model:v2');
  await user.click(screen.getByRole('button', { name: 'Add' }));
  expect(submit).toHaveBeenCalledWith('private/model:v2');
});
