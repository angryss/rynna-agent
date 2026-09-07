import { newSessionId } from '../sessions';
import { useEffect, useRef, useState } from 'react';
import type { AgentClient, ModelSelection, WorkflowAction, WorkflowLimits, WorkflowMetadata, WorkflowRun, WorkflowStart } from '../contracts';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';
function contextSuffix(context: string): string {
  const bytes = new TextEncoder().encode(context);
  let start = Math.max(0, bytes.length - 16000);
  // Skip continuation bytes so the suffix starts at a complete UTF-8 character.
  while (start < bytes.length && (bytes[start]! & 0xc0) === 0x80) start++;
  return new TextDecoder().decode(bytes.subarray(start));
}
export const workflowTerminal = (run: WorkflowRun) => ['completed', 'failed', 'cancelled', 'budget_exhausted'].includes(run.status);
interface Props { disabled?: boolean; client: AgentClient; profile: string; session: string; project: string | null; selection: ModelSelection; selected?: string; context: string; savedRunId?: string; onSelection(id: string): void; onRun(run: WorkflowRun): void }
export function WorkflowPanel(props: Props) {
  const { client, profile, session, selected = '', project, selection, disabled = false } = props;
  const latest = useRef(props); latest.current = props;
  const [items, setItems] = useState<WorkflowMetadata[]>([]);
  const [run, setRun] = useState<WorkflowRun | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [goal, setGoal] = useState('');
  const [criteria, setCriteria] = useState('');
  const [limits, setLimits] = useState<WorkflowLimits>({ steps: 50, tool_calls: 512, active_seconds: 1800 });
  const [steering, setSteering] = useState('');
  const [amendment, setAmendment] = useState('');
  const mounted = useRef(true);
  const observed = useRef<WorkflowRun | null>(null);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const [acknowledge, setAcknowledge] = useState(false);
  const request = useRef<WorkflowStart | null>(null);
  function accept(next: WorkflowRun) { if (!mounted.current || (observed.current?.id === next.id && observed.current.revision > next.revision)) return; observed.current = next; if (workflowTerminal(next)) request.current = null; setRun(next); latest.current.onRun(next); }
  useEffect(() => { let active = true; void client.listWorkflows?.(profile).then(v => { if (active) setItems(v); }).catch(e => { if (active) setError(String(e)); }); return () => { active = false; }; }, [client, profile]);
  useEffect(() => {
    let active = true; let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const runs = await client.listWorkflowRuns!(profile, session);
        if (!active) return;
        const current = runs.find(r => !workflowTerminal(r)) ?? runs.at(-1);
        if (current) { accept(current); setError(''); }
        else if (latest.current.savedRunId || observed.current) setError('Saved workflow run is unavailable on this host. Cached progress is not current.');
        else setError('');
      } catch (e) { if (active) setError(`Host state unavailable: ${String(e)}`); }
      if (active) timer = setTimeout(() => void poll(), 2000);
    }
    void poll(); return () => { active = false; clearTimeout(timer); };
  }, [client, profile, session]);
  async function action(action: WorkflowAction) {
    if (!run || busy || latest.current.disabled) return; setBusy(true); setError('');
    try { accept(await client.controlWorkflow!(run.id, { ...action, profile, session_id: session, expected_revision: run.revision })); }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  async function start() {
    if (busy || latest.current.disabled) return; setBusy(true); setError('');
    try {
      // Retain the exact request ID after a lost response. Polling can also recover by session.
      request.current ??= { request_id: newSessionId(), session_id: session, profile, project, selection, workflow_id: selected, goal, criteria: criteria.split('\n').filter(v => v.trim()).map((text, i) => ({ id: `criterion-${i + 1}`, text })), limits, initial_context: contextSuffix(latest.current.context) };
      accept(await client.startWorkflow!(request.current));
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  const canStart = !run || workflowTerminal(run);
  return <section className="workflow-panel" aria-label="Workflow">
    <label>Workflow<select disabled={disabled} value={selected} onChange={e => { request.current = null; latest.current.onSelection(e.target.value); }}><option value="">Chat</option>{items.map(w => <option key={w.id} value={w.id}>{w.name}</option>)}</select></label>
    {selected && canStart && <form className="provider-form" onSubmit={e => { e.preventDefault(); void start(); }}>
      <label>Goal<Textarea required maxLength={8192} value={goal} onChange={e => { request.current = null; setGoal(e.target.value); }} /></label>
      <label>Success criteria · one per line<Textarea required value={criteria} onChange={e => { request.current = null; setCriteria(e.target.value); }} /></label>
      <div className="workflow-limits">{([['steps', 'Step limit', 50], ['tool_calls', 'Tool call limit', 512], ['active_seconds', 'Active seconds limit', 1800]] as const).map(([key, label, max]) => <label key={key}>{label}<Input type="number" min={1} max={max} required value={limits[key]} onChange={e => { request.current = null; setLimits({ ...limits, [key]: Number(e.target.value) }); }} /></label>)}</div>
      <Button disabled={disabled || busy || !criteria.trim()} type="submit">Start workflow</Button>
    </form>}
    {run && <div className="workflow-progress">
      <p role="status"><strong>{error ? 'Last known: ' : ''}{run.status.replaceAll('_', ' ')}</strong> · {run.workflow.steps[run.cursor]?.id}</p><p>{run.start.goal}</p><ul>{run.start.criteria.map(c => <li key={c.id}>{c.text}</li>)}</ul>{run.reason && <p>{run.reason}</p>}
      <p>Steps {run.consumed.steps}/{run.start.limits.steps} · Tools {run.consumed.tool_calls}/{run.start.limits.tool_calls} · Active seconds {run.consumed.active_seconds}/{run.start.limits.active_seconds}</p>
      {run.verification && <div><p>{run.verification.summary}</p><ul>{run.verification.results.map(e => <li key={e.criterion_id}><strong>{e.criterion_id}: {e.verdict}</strong> · {e.kind}<p>{e.reference} {e.excerpt}</p></li>)}</ul></div>}
      {run.uncertain && <label><input type="checkbox" checked={acknowledge} onChange={e => setAcknowledge(e.target.checked)} /> The interrupted step may have changed files or other resources. I acknowledge that retry can repeat side effects.</label>}
      <div className="provider-actions">
        {run.status === 'running' && <Button disabled={disabled || busy || !!error} onClick={() => void action({ action: 'pause' })}>Pause</Button>}
        {['paused', 'blocked'].includes(run.status) && <Button disabled={disabled || busy || !!error || (run.uncertain && !acknowledge)} onClick={() => void action({ action: 'resume', acknowledge_uncertain: acknowledge })}>Resume</Button>}
        {!workflowTerminal(run) && <Button disabled={disabled || busy || run.status === 'cancelling'} variant="outline" onClick={() => void action({ action: 'cancel' })}>{['running', 'pausing', 'cancelling'].includes(run.status) ? (run.status === 'cancelling' ? 'Stopping…' : 'Stop') : 'Cancel run'}</Button>}
      </div>
      {!workflowTerminal(run) && <form className="provider-form" onSubmit={e => { e.preventDefault(); void action({ action: 'steer', text: steering, criteria: amendment.trim() ? amendment.split('\n').filter(v => v.trim()).map((text, i) => ({ id: run.start.criteria[i]?.id ?? `criterion-${i + 1}`, text })) : null }).then(() => setSteering('')); }}>
        <label>Steering<Textarea value={steering} onChange={e => setSteering(e.target.value)} maxLength={8192} /></label>
        <label>Amend success criteria · optional, one per line<Textarea value={amendment} onChange={e => setAmendment(e.target.value)} placeholder={run.start.criteria.map(c => c.text).join("\n")} /></label>
        {run.status === 'running' ? <Button type="button" disabled={disabled || busy} onClick={() => void action({ action: 'pause' })}>Pause to steer</Button> : <Button type="submit" disabled={disabled || busy || !steering.trim() || !['paused', 'blocked'].includes(run.status)}>Record steering</Button>}
        <p>Pause, record your changes, then resume. Steering does not launch an ordinary response.</p>
      </form>}
    </div>}
    {error && <p className="request-error" role="alert">{error}</p>}
  </section>;
}
