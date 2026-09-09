import { fireEvent, render, screen } from '@testing-library/react';
import { expect, it } from 'vitest';
import { ThinkingContent } from './thinking-content';

it('follows new thinking, pauses when scrolled up, and resumes at the bottom', () => {
  const { rerender } = render(<ThinkingContent content="first" expanded />);
  const viewport = screen.getByLabelText('Thinking content');
  Object.defineProperties(viewport, {
    scrollHeight: { configurable: true, value: 1000 },
    clientHeight: { configurable: true, value: 160 },
  });
  rerender(<ThinkingContent content="first second" expanded />);
  expect(viewport.scrollTop).toBe(1000);
  viewport.scrollTop = 200;
  fireEvent.scroll(viewport);
  rerender(<ThinkingContent content="first second third" expanded />);
  expect(viewport.scrollTop).toBe(200);
  viewport.scrollTop = 840;
  fireEvent.scroll(viewport);
  rerender(<ThinkingContent content="first second third fourth" expanded />);
  expect(viewport.scrollTop).toBe(1000);
});

it('scrolls to the latest lines when opened and preserves all text', () => {
  const content = Array.from({ length: 30 }, (_, i) => `Line ${i}`).join('\n');
  const { rerender } = render(<ThinkingContent content={content} expanded={false} />);
  const viewport = screen.getByLabelText('Thinking content');
  Object.defineProperty(viewport, 'scrollHeight', { value: 1000 });
  expect(viewport.tabIndex).toBe(-1);
  rerender(<ThinkingContent content={content} expanded />);
  expect(viewport.scrollTop).toBe(1000);
  expect(viewport.tabIndex).toBe(0);
  expect(viewport.textContent).toBe(content);
});
