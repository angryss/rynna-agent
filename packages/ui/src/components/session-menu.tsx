import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { MoreVertical, Trash2 } from 'lucide-react';

export function SessionMenu({ name, disabled, onDelete }: {
  name: string;
  disabled: boolean;
  onDelete: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const item = useRef<HTMLButtonElement>(null);
  const id = useId();
  const visible = open && !disabled;

  function close(restoreFocus = true) {
    setOpen(false);
    if (restoreFocus) trigger.current?.focus();
  }

  useEffect(() => { setOpen(false); }, [disabled]);
  useLayoutEffect(() => {
    if (!visible) return;
    const anchor = trigger.current!.getBoundingClientRect();
    const panel = menu.current!.getBoundingClientRect();
    setPosition({
      left: Math.max(8, Math.min(anchor.right - panel.width, window.innerWidth - panel.width - 8)),
      top: anchor.bottom + panel.height + 12 <= window.innerHeight
        ? anchor.bottom + 4 : Math.max(8, anchor.top - panel.height - 4),
    });
    item.current?.focus();
  }, [visible]);

  useEffect(() => {
    if (!visible) return;
    const outside = (event: Event) => {
      if (event.target instanceof Node && !menu.current?.contains(event.target) && !trigger.current?.contains(event.target)) close(false);
    };
    const dismiss = () => close(false);
    document.addEventListener('pointerdown', outside);
    document.addEventListener('focusin', outside);
    window.addEventListener('resize', dismiss);
    window.addEventListener('scroll', dismiss, true);
    return () => {
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('focusin', outside);
      window.removeEventListener('resize', dismiss);
      window.removeEventListener('scroll', dismiss, true);
    };
  }, [visible]);

  return <>
    <button ref={trigger} className="session-more" type="button" disabled={disabled}
      aria-label={`More options for ${name}`} title="More options"
      aria-haspopup="menu" aria-expanded={visible} aria-controls={visible ? id : undefined}
      onClick={() => setOpen(!open)} onKeyDown={event => {
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); setOpen(true); }
      }}>
      <MoreVertical aria-hidden="true" size={16} />
    </button>
    {visible && createPortal(
      <div ref={menu} id={id} role="menu" aria-label={`Options for ${name}`} className="session-more-menu"
        style={position} onKeyDown={event => {
          if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); }
          if (event.key === 'Tab') close();
          if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) { event.preventDefault(); item.current?.focus(); }
        }}>
        <button ref={item} role="menuitem" type="button" onClick={() => { close(); onDelete(); }}>
          <Trash2 aria-hidden="true" size={15} />Delete
        </button>
      </div>, document.body)}
  </>;
}
