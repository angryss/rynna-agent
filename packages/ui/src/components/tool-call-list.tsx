import { useEffect, useState } from 'react';
import type { ToolCallActivity } from '../sessions';

function label(name: string): string {
  if (name === 'run_command') return 'Terminal';
  return name.replace(/([a-z])([A-Z])/g, '$1 $2').replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, character => character.toUpperCase());
}

function preview(call: ToolCallActivity): string {
  const args = call.arguments;
  if (!args || typeof args !== 'object' || Array.isArray(args)) return typeof args === 'string' ? args : '';
  const values = args as Record<string, unknown>;
  if (call.name === 'run_command' && typeof values.program === 'string') {
    return [values.program, ...(Array.isArray(values.arguments) ? values.arguments.filter((arg): arg is string => typeof arg === 'string') : [])]
      .map(arg => /^[a-zA-Z0-9_./:=@%+,-]+$/.test(arg) ? arg : JSON.stringify(arg)).join(' ');
  }
  for (const key of ['command', 'name', 'path', 'file_path', 'query', 'pattern', 'url', 'prompt']) {
    if (typeof values[key] === 'string') return values[key];
  }
  return '';
}

export function ToolCallList({ calls }: { calls: ToolCallActivity[] }) {
  const [now, setNow] = useState(Date.now);
  const running = calls.some(call => call.status === 'running');
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => setNow(Date.now()), 100);
    return () => window.clearInterval(timer);
  }, [running]);
  if (!calls.length) return null;
  return <section className="tool-call-list" aria-label="Tool calls">
    <h3>Tool calls ({calls.length})</h3>
    <ul>{calls.map((call, index) => {
      const argument = preview(call).replace(/\s+/g, ' ').trim();
      const short = argument.length > 100 ? `${argument.slice(0, 99)}…` : argument;
      const status = call.status.charAt(0).toUpperCase() + call.status.slice(1);
      const elapsed = Math.max(0, call.elapsed_ms ?? now - call.started_at);
      return <li className={`tool-call tool-call-${call.status}`} key={`${index}-${call.id}`}>
        <details>
          <summary>
            <span className="tool-call-dot" aria-hidden="true">●</span>
            <span className="tool-call-label"><strong>{label(call.name)}</strong>{short && <code>({short})</code>}</span>
            <span className="tool-call-elapsed">({(elapsed / 1000).toFixed(1)}s)</span>
            <span className="tool-call-status" role={call.status === 'running' ? 'status' : undefined}>{status}</span>
          </summary>
          <pre>{JSON.stringify(call.arguments, null, 2) ?? 'No arguments'}</pre>
        </details>
      </li>;
    })}</ul>
  </section>;
}
