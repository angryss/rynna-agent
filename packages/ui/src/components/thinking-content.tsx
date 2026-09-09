import { useLayoutEffect, useRef } from 'react';

export function ThinkingContent({ content, expanded }: { content: string; expanded: boolean }) {
  const viewport = useRef<HTMLParagraphElement>(null);
  const followTail = useRef(true);

  useLayoutEffect(() => {
    if (expanded && followTail.current && viewport.current) {
      viewport.current.scrollTop = viewport.current.scrollHeight;
    }
  }, [content, expanded]);

  return <p ref={viewport} tabIndex={expanded ? 0 : -1} aria-label="Thinking content"
    onScroll={(event) => {
      const element = event.currentTarget;
      followTail.current = element.scrollHeight - element.clientHeight - element.scrollTop <= 2;
    }}>{content}</p>;
}
