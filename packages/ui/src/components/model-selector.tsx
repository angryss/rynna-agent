import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, ChevronDown, Search } from 'lucide-react';
import type { ModelSelection, Profile } from '../contracts';

const levels = ['default', 'low', 'medium', 'high'] as const;
const levelNames = { default: 'Default', low: 'Low', medium: 'Med', high: 'High' };

export function ModelSelector({ profile, selection, disabled, onChange, openRequest = 0 }: {
  openRequest?: number;
  profile: Profile;
  selection?: ModelSelection;
  disabled: boolean;
  onChange: (selection?: ModelSelection) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [position, setPosition] = useState({ top: 0, left: 0, maxHeight: 400, width: 360 });
  const previousOpenRequest = useRef(openRequest);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const id = useId();
  const pairs = profile.providers.filter(pair => pair.enabled !== false);
  const active = selection ?? pairs.find(pair => pair.default) ?? pairs[0];
  const thinking = selection?.thinking ?? 'default';
  const filtered = pairs.filter(pair => `${pair.provider} ${pair.model}`.toLowerCase().includes(query.trim().toLowerCase()));
  const providers = [...new Set(filtered.map(pair => pair.provider))];
  const visible = open && !disabled;

  function close(restoreFocus = true) {
    setOpen(false);
    if (restoreFocus) trigger.current?.focus();
  }

  useEffect(() => { setOpen(false); }, [disabled, profile.name]);
  useEffect(() => {
    if (openRequest !== previousOpenRequest.current) { setQuery(''); setOpen(true); }
    previousOpenRequest.current = openRequest;
  }, [openRequest]);

  useLayoutEffect(() => {
    if (!visible) return;
    const place = () => {
      const anchor = trigger.current?.getBoundingClientRect();
      if (!anchor) return;
      const width = Math.min(360, window.innerWidth - 24);
      const above = anchor.top - 20;
      const below = window.innerHeight - anchor.bottom - 20;
      const up = above >= Math.min(400, below);
      const maxHeight = Math.min(400, Math.max(100, up ? above : below));
      const height = Math.min(panel.current?.scrollHeight ?? maxHeight, maxHeight);
      setPosition({ width, maxHeight, left: Math.max(12, Math.min(anchor.right - width, window.innerWidth - width - 12)),
        top: up ? Math.max(12, anchor.top - height - 8) : anchor.bottom + 8 });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [visible, query, position.width, position.maxHeight]);

  useEffect(() => {
    if (!visible) return;
    search.current?.focus();
    const outside = (event: Event) => {
      if (event.target instanceof Node && !panel.current?.contains(event.target) && !trigger.current?.contains(event.target)) close(false);
    };
    document.addEventListener('pointerdown', outside);
    document.addEventListener('focusin', outside);
    return () => {
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('focusin', outside);
    };
  }, [visible]);

  return <>
    <button ref={trigger} type="button" className="model-picker-trigger" disabled={disabled || !active}
      aria-label={`Choose model: ${active?.model ?? 'No enabled models'} · ${levelNames[thinking]}`}
      title={active ? `${active.provider} / ${active.model}${selection ? '' : ' (profile default)'}` : 'No enabled models'}
      aria-haspopup="dialog" aria-expanded={visible} aria-controls={visible ? id : undefined}
      onClick={() => { setQuery(''); setOpen(!open); }}>
      <span>{active?.model ?? 'No enabled models'}</span><span className="model-picker-effort">· {levelNames[thinking]}</span>
      <ChevronDown size={14} aria-hidden="true" />
    </button>
    {visible && createPortal(
      <div ref={panel} id={id} role="dialog" aria-label="Choose provider and model" className="model-picker-panel"
        style={position} onKeyDown={event => {
          if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); }
          if (event.key === 'Enter' && event.target === search.current) {
            event.preventDefault();
            panel.current?.querySelector<HTMLButtonElement>('[data-model-option]')?.click();
          }
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            if (event.target instanceof HTMLInputElement && event.target.type === 'radio') return;
            event.preventDefault();
            const options = Array.from(panel.current?.querySelectorAll<HTMLButtonElement>('[data-model-option]') ?? []);
            const index = options.indexOf(document.activeElement as HTMLButtonElement);
            const next = index < 0 ? (event.key === 'ArrowDown' ? 0 : options.length - 1) : (index + (event.key === 'ArrowDown' ? 1 : -1) + options.length) % options.length;
            options[next]?.focus();
          }
        }}>
        <div className="model-picker-search"><Search size={16} aria-hidden="true" />
          <input ref={search} type="search" aria-label="Search models" placeholder="Search models…" value={query}
            onChange={event => setQuery(event.target.value)} />
        </div>
        <div className="model-picker-results">
          {!query.trim() && <button type="button" data-model-option aria-pressed={!selection} className="model-picker-option"
            onClick={() => { onChange(undefined); close(); }}>
            <span>Profile default<small>Use the profile’s default and fallback models</small></span>
            {!selection && <Check size={16} aria-hidden="true" />}
          </button>}
          {providers.map(provider => <section key={provider} aria-label={provider}>
            <h3>{provider}</h3>
            {filtered.filter(pair => pair.provider === provider).map(pair => {
              const selected = selection?.provider === pair.provider && selection.model === pair.model;
              return <button key={pair.model} type="button" data-model-option className="model-picker-option" aria-pressed={selected}
                onClick={() => { onChange({ provider: pair.provider, model: pair.model, thinking: selected ? thinking : 'default' }); close(); }}>
                <span>{pair.model}</span>{selected && <Check size={16} aria-hidden="true" />}
              </button>;
            })}
          </section>)}
          {filtered.length === 0 && <p className="model-picker-empty">No models match “{query}”.</p>}
        </div>
        <fieldset className="model-picker-thinking"><legend>Thinking level</legend>
          {levels.map(level => <label key={level}>
            <input type="radio" name={`${id}-thinking`} value={level} checked={thinking === level} disabled={!active}
              onChange={() => { if (active) onChange({ provider: active.provider, model: active.model, thinking: level }); }} />
            <span>{level === 'medium' ? 'Medium' : levelNames[level]}</span>
          </label>)}
        </fieldset>
      </div>, document.body)}
  </>;
}
