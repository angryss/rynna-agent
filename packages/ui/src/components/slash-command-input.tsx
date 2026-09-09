import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Terminal } from 'lucide-react';

export const slashCommands = [
  { name: '/new', description: 'Start a new chat in the current project' },
  { name: '/clear', description: 'Start fresh while keeping saved chats' },
  { name: '/retry', description: 'Resend the last user message' },
  { name: '/title', description: 'Rename this chat: /title <name>' },
  { name: '/save', description: 'Download this conversation as JSON' },
  { name: '/model', description: 'Choose a model and thinking level' },
  { name: '/settings', description: 'Open Settings' },
  { name: '/help', description: 'Show available commands' },
  { name: '/compact', description: 'Summarize context while keeping the transcript' },
] as const;

export function SlashCommandInput({ value, onChange, onCommand, busy }: {
  value: string;
  onChange: (value: string) => void;
  onCommand: (value: string) => void;
  busy: boolean;
}) {
  const input = useRef<HTMLTextAreaElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const id = useId();
  const [dismissed, setDismissed] = useState(false);
  const [focused, setFocused] = useState(false);
  const [active, setActive] = useState(0);
  const [position, setPosition] = useState({ left: 12, bottom: 0, width: 400, maxHeight: 320 });
  const query = value.trimStart().toLowerCase();
  const matches = slashCommands.filter(command => command.name.startsWith(query));
  const visible = focused && !busy && !dismissed && /^\/\S*$/.test(query);
  const selected = Math.min(active, Math.max(0, matches.length - 1));

  useEffect(() => { setActive(0); setDismissed(false); }, [value]);
  useEffect(() => {
    if (visible) panel.current?.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: 'nearest' });
  }, [selected, visible]);
  useLayoutEffect(() => {
    if (!visible) return;
    const place = () => {
      const anchor = input.current?.getBoundingClientRect();
      if (!anchor) return;
      const width = Math.min(560, window.innerWidth - 24, anchor.width);
      setPosition({ left: Math.max(12, Math.min(anchor.left, window.innerWidth - width - 12)),
        bottom: window.innerHeight - anchor.top + 8, width, maxHeight: Math.max(0, Math.min(360, anchor.top - 20)) });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [visible]);

  function choose(name: string, complete: boolean) {
    if (name === '/help' && !complete) { onChange('/'); setDismissed(false); input.current?.focus(); return; }
    if (complete || name === '/title') onChange(`${name} `);
    else onCommand(name);
    setDismissed(true);
    input.current?.focus();
  }

  return <>
    <textarea ref={input} id="prompt" name="prompt" className="slash-command-input" rows={3}
      value={value} placeholder="Describe the task, or type / for commands…"
      aria-autocomplete="list" aria-controls={visible ? id : undefined}
      aria-activedescendant={visible && matches.length ? `${id}-${selected}` : undefined}
      onFocus={() => setFocused(true)} onBlur={() => setFocused(false)}
      onChange={event => onChange(event.target.value)}
      onKeyDown={event => {
        if (event.nativeEvent.isComposing || event.shiftKey || event.altKey || event.ctrlKey || event.metaKey) return;
        if (visible) {
          if (event.key === 'Escape') { event.preventDefault(); setDismissed(true); return; }
          if (matches.length && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
            event.preventDefault();
            setActive((selected + (event.key === 'ArrowDown' ? 1 : -1) + matches.length) % matches.length);
            return;
          }
          if (matches.length && (event.key === 'Tab' || event.key === 'Enter')) {
            event.preventDefault(); choose(matches[selected]!.name, event.key === 'Tab'); return;
          }
        }
        if (event.key === 'Enter') { event.preventDefault(); event.currentTarget.form?.requestSubmit(); }
      }} />
    {visible && createPortal(<div ref={panel} className="slash-command-panel" style={position}>
      <div className="slash-command-heading">Commands <span>↑↓ Navigate · Tab Complete · Enter Run</span></div>
      <div id={id} role="listbox" aria-label="Slash commands" className="slash-command-results">
        {matches.map((command, index) => <div key={command.name} id={`${id}-${index}`} role="option"
          aria-selected={index === selected} className="slash-command-option"
          onPointerMove={() => setActive(index)} onPointerDown={event => event.preventDefault()}
          onClick={() => choose(command.name, false)}>
          <Terminal size={18} aria-hidden="true" /><strong>{command.name}</strong><span>{command.description}</span>
        </div>)}
        {!matches.length && <p className="slash-command-empty">No matching commands. Type /help to see the list.</p>}
      </div>
    </div>, document.body)}
  </>;
}
