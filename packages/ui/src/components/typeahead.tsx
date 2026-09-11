import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

import { Input } from './ui/input';

export interface TypeaheadProps {
  disabled?: boolean;
  allowCustom?: boolean;
  placeholder?: string;
  required?: boolean;
  descriptionId?: string;
  id: string;
  onChange: (value: string) => void;
  options: readonly string[];
  value: string;
}

export function Typeahead({
  disabled,
  id,
  onChange,
  options,
  value,
  allowCustom = false,
  placeholder,
  required,
  descriptionId,
}: TypeaheadProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({
    left: 0,
    top: 0,
    width: 0,
    maxHeight: 256,
    upward: false,
  });
  const [query, setQuery] = useState(value);
  const [open, setOpen] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [active, setActive] = useState<string | null>(value || null);

  useEffect(() => {
    setQuery(value);
    setDirty(false);
    setActive(allowCustom ? null : value || null);
  }, [value, allowCustom]);

  const matches = matchingOptions(options, query, !dirty && !allowCustom);

  const showOptions = open && (!allowCustom || matches.length > 0);

  useLayoutEffect(() => {
    if (!allowCustom || !showOptions) return;
    const place = () => {
      const rect = containerRef.current?.querySelector('input')?.getBoundingClientRect();
      if (!rect) return;
      const below = window.innerHeight - rect.bottom - 8;
      const above = rect.top - 8;
      const upward = below < 160 && above > below;
      const maxHeight = Math.max(40, Math.min(256, upward ? above : below));
      setPosition({
        left: rect.left,
        width: rect.width,
        top: upward ? rect.top - 4 : rect.bottom + 4,
        maxHeight,
        upward,
      });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [allowCustom, showOptions, matches.length]);

  useEffect(() => {
    if (allowCustom && showOptions && active)
      document
        .getElementById(`${id}-option-${matches.indexOf(active)}`)
        ?.scrollIntoView?.({ block: 'nearest' });
  }, [allowCustom, showOptions, active, id, matches.indexOf(active ?? '')]);

  function cancelEditing() {
    setQuery(value);
    setOpen(false);
    setDirty(false);
    setActive(value || null);
  }

  function select(next: string) {
    onChange(next);
    setQuery(next);
    setOpen(false);
    setDirty(false);
    setActive(next);
  }

  const optionList = (
    <div
      style={
        allowCustom
          ? {
              left: position.left,
              top: position.top,
              width: position.width,
              maxHeight: position.maxHeight,
              transform: position.upward ? 'translateY(-100%)' : undefined,
              position: 'fixed',
              marginTop: 0,
            }
          : undefined
      }
      className={allowCustom ? 'provider-type-options model-type-options' : 'provider-type-options'}
      id={`${id}-options`}
      role="listbox"
    >
      {matches.map((option, index) => (
        <button
          aria-selected={option === active}
          id={`${id}-option-${index}`}
          key={option}
          onClick={() => select(option)}
          onMouseDown={(event) => event.preventDefault()}
          onMouseMove={() => setActive(option)}
          role="option"
          tabIndex={-1}
          type="button"
        >
          {option}
        </button>
      ))}
    </div>
  );

  return (
    <div ref={containerRef} className="provider-typeahead">
      <Input
        aria-activedescendant={
          open && active && matches.includes(active)
            ? `${id}-option-${matches.indexOf(active)}`
            : undefined
        }
        aria-autocomplete="list"
        aria-controls={`${id}-options`}
        aria-expanded={showOptions}
        autoComplete="off"
        placeholder={placeholder}
        required={required}
        aria-describedby={descriptionId}
        disabled={disabled}
        id={id}
        onBlur={allowCustom ? () => setOpen(false) : cancelEditing}
        onChange={(event) => {
          const nextQuery = event.target.value;
          setQuery(nextQuery);
          setOpen(true);
          setDirty(true);
          if (allowCustom) onChange(nextQuery);
          setActive(allowCustom ? null : (matchingOptions(options, nextQuery, false)[0] ?? null));
        }}
        onFocus={(event) => {
          event.currentTarget.select();
          setOpen(true);
          const nextMatches = matchingOptions(options, query, !dirty && !allowCustom);
          setActive(
            allowCustom ? null : nextMatches.includes(value) ? value : (nextMatches[0] ?? null),
          );
        }}
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            if (allowCustom) {
              setOpen(false);
              setActive(null);
              return;
            }
            cancelEditing();
            return;
          }
          const nextMatches = matchingOptions(options, query, !dirty && !allowCustom);
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            if (nextMatches.length === 0) return;
            event.preventDefault();
            setOpen(true);
            const currentIndex = active ? nextMatches.indexOf(active) : -1;
            const offset = event.key === 'ArrowDown' ? 1 : -1;
            const nextIndex =
              currentIndex === -1
                ? event.key === 'ArrowDown'
                  ? 0
                  : nextMatches.length - 1
                : (currentIndex + offset + nextMatches.length) % nextMatches.length;
            setActive(nextMatches[nextIndex] ?? null);
            return;
          }
          if (event.key !== 'Enter') return;
          if (allowCustom && (!open || !active)) return;
          const match = (active && nextMatches.includes(active) ? active : nextMatches[0]) ?? null;
          if (!match) return;
          event.preventDefault();
          select(match);
        }}
        role="combobox"
        value={query}
      />
      {showOptions ? (allowCustom ? createPortal(optionList, document.body) : optionList) : null}
    </div>
  );
}

function matchingOptions(options: readonly string[], query: string, showAll: boolean): string[] {
  return [...options]
    .sort((left, right) => left.localeCompare(right))
    .filter((option) => showAll || option.toLowerCase().includes(query.toLowerCase()));
}
