import { useEffect, useState } from 'react';
import type { AgentClient, Profile, Workflow, WorkflowMetadata, WorkflowStep } from '../contracts';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';
export function WorkflowSettings({ client, profile }: { client: AgentClient; profile: Profile }) {
  const [items, setItems] = useState<WorkflowMetadata[]>([]);
  const [draft, setDraft] = useState<Workflow | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const readOnly = draft?.id === 'rynna-default';
  useEffect(() => { let active = true; void client.listWorkflows?.(profile.name).then(v => { if (active) setItems(v); }).catch(e => { if (active) setError(String(e)); }); return () => { active = false; }; }, [client, profile.name]);
  async function perform(action: () => Promise<void>) { setBusy(true); setError(''); try { await action(); setItems(await client.listWorkflows!(profile.name)); } catch (e) { setError(String(e)); } finally { setBusy(false); } }
  function updateStep(index: number, patch: Partial<WorkflowStep>) { if (draft) setDraft({ ...draft, steps: draft.steps.map((s, i) => i === index ? { ...s, ...patch } : s) }); }
  function duplicate(workflow: Workflow) { setEditingId(null); setDraft({ ...workflow, id: `custom-${Date.now()}`, name: `${workflow.name} copy`, revision: 1 }); }
  return <div className="project-settings workflow-settings">
    <div className="settings-heading"><div><h2>Workflows</h2><p>Define a repeatable process for this profile. Existing runs retain their captured steps.</p></div>
      <Button disabled={busy} type="button" onClick={() => void perform(async () => duplicate(await client.readWorkflow!(profile.name, 'rynna-default')))}>Add workflow</Button></div>
    {items.map(item => <article className="project-row" key={item.id}><div><h3>{item.name}{item.read_only ? ' · Built-in' : ''}</h3><p>{item.description}</p></div><div className="provider-actions">
      <Button disabled={busy} variant="outline" onClick={() => void perform(async () => { const workflow = await client.readWorkflow!(profile.name, item.id); setEditingId(workflow.id); setDraft(workflow); })}>{item.read_only ? 'View' : 'Edit'}</Button>
      <Button disabled={busy} variant="outline" onClick={() => void perform(async () => duplicate(await client.readWorkflow!(profile.name, item.id)))}>Duplicate</Button>
      {!item.read_only && <Button disabled={busy} variant="ghost" onClick={() => void perform(async () => { await client.deleteWorkflow!(profile.name, item.id); setDraft(null); })}>Delete</Button>}
    </div></article>)}
    {draft && <form className="provider-form" onSubmit={e => { e.preventDefault(); if (editingId === null && items.some(w => w.id === draft.id)) { setError('This workflow ID already exists. Choose a different ID.'); return; } void perform(async () => { await client.saveWorkflow!(profile.name, draft); setDraft(null); }); }}>
      <h3>{readOnly ? 'Built-in workflow · read-only' : 'Edit workflow'}</h3>
      <label>ID<Input required pattern="[A-Za-z0-9_-]{1,64}" disabled={busy || readOnly || editingId !== null} value={draft.id} onChange={e => setDraft({ ...draft, id: e.target.value })} /></label>
      <label>Name<Input required maxLength={256} disabled={busy || readOnly} value={draft.name} onChange={e => setDraft({ ...draft, name: e.target.value })} /></label>
      <label>Description<Textarea maxLength={1024} disabled={busy || readOnly} value={draft.description} onChange={e => setDraft({ ...draft, description: e.target.value })} /></label>
      <ol className="workflow-steps">{draft.steps.map((step, index) => <li key={index}><fieldset disabled={busy || readOnly}><legend>Step {index + 1} · {step.role === 'verify' ? 'Verify' : 'Work'}</legend>
        <label>Step ID<Input required pattern="[A-Za-z0-9_-]{1,64}" value={step.id} onChange={e => updateStep(index, { id: e.target.value })} /></label>
        <label>Executor<select value={step.executor} onChange={e => updateStep(index, { executor: e.target.value as WorkflowStep['executor'], helper: e.target.value === 'subagent' ? profile.subagents[0]?.name : undefined })}><option value="instructions">Direct agent</option><option value="subagent">Named helper</option></select></label>
        {step.executor === 'subagent' && <label>Helper<select required value={step.helper ?? ''} onChange={e => updateStep(index, { helper: e.target.value })}><option value="">Select helper</option>{profile.subagents.map(h => <option key={h.name}>{h.name}</option>)}</select></label>}
        <label>Instructions<Textarea required maxLength={32000} rows={4} value={step.instructions} onChange={e => updateStep(index, { instructions: e.target.value })} /></label>
        {step.role === 'verify' ? <label>Repeat from<select required value={step.repeat_target} onChange={e => updateStep(index, { repeat_target: e.target.value })}>{draft.steps.filter(s => s.role === 'work').map(s => <option key={s.id}>{s.id}</option>)}</select></label> : <div className="provider-actions">
          <Button type="button" variant="outline" disabled={index === 0} onClick={() => { const steps = [...draft.steps]; [steps[index - 1], steps[index]] = [steps[index]!, steps[index - 1]!]; setDraft({ ...draft, steps }); }}>Move up</Button>
          <Button type="button" variant="ghost" disabled={draft.steps.length <= 2} onClick={() => setDraft({ ...draft, steps: draft.steps.filter((_, i) => i !== index) })}>Remove step</Button></div>}
      </fieldset></li>)}</ol>
      {!readOnly && <div className="provider-actions"><Button type="button" variant="outline" disabled={busy || draft.steps.length >= 16} onClick={() => setDraft({ ...draft, steps: [...draft.steps.slice(0, -1), { id: `step-${Date.now()}`, role: 'work', executor: 'instructions', instructions: '' }, draft.steps.at(-1)!] })}>Add work step</Button><Button disabled={busy} type="submit">Save workflow</Button></div>}
      <Button type="button" variant="ghost" onClick={() => setDraft(null)}>Close editor</Button>
    </form>}
    {error && <p className="request-error" role="alert">{error}</p>}
  </div>;
}
