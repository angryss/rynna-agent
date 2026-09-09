import { type FormEvent, useEffect, useRef, useState } from 'react';
import { type AgentClient, type McpServer, isMcpSettings } from '../contracts';
import { Button } from './ui/button';
import { Textarea } from './ui/textarea';

export function McpSettingsPanel({ client, profile }: { client: AgentClient; profile: string }) {
  return <McpEditor key={profile} client={client} profile={profile} />;
}

function McpEditor({ client, profile }: { client: AgentClient; profile: string }) {
  const [draft, setDraft] = useState('{\n  "mcpServers": {}\n}');
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  const active = useRef(true);

  useEffect(() => {
    active.current = true;
    let current = true;
    setLoading(true);
    setLoaded(false);
    setError('');
    void client.getMcpSettings!(profile).then(settings => {
      if (!current) return;
      setDraft(JSON.stringify(settings, null, 2));
      setLoaded(true);
    }).catch(() => {
      if (current) setError('Could not load MCP settings. Retry loading; if mcp.yaml is invalid, repair it first.');
    }).finally(() => { if (current) setLoading(false); });
    return () => { current = false; active.current = false; };
  }, [client, profile, attempt]);

  function parse() {
    let value: unknown;
    try { value = JSON.parse(draft); } catch { throw new Error('Enter valid JSON. Check commas, quotes, and brackets.'); }
    if (!isMcpSettings(value)) throw new Error('Use an mcpServers object with named stdio or streamable_http server configurations.');
    return value;
  }

  function addExample(name: string, server: McpServer) {
    try {
      const value = parse();
      let key = name;
      for (let index = 2; key in value.mcpServers; index++) key = `${name}-${index}`;
      value.mcpServers[key] = server;
      setDraft(JSON.stringify(value, null, 2));
      setError(''); setStatus('');
    } catch (reason) { setError((reason as Error).message); }
  }

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!loaded || loading || saving || !client.saveMcpSettings) return;
    setError(''); setStatus('');
    try {
      const settings = parse();
      setSaving(true);
      const saved = await client.saveMcpSettings(settings, profile);
      if (!active.current) return;
      setDraft(JSON.stringify(saved, null, 2));
      setStatus(`MCP servers saved for ${profile}. Changes apply to the next request.`);
    } catch (reason) {
      if (active.current) setError(reason instanceof Error ? reason.message : 'Could not save MCP settings.');
    } finally { if (active.current) setSaving(false); }
  }

  return <>
    <div className="settings-heading"><div>
      <h2>MCP servers</h2>
      <p>Connect tools for {profile}. Each profile has its own server list.</p>
    </div></div>
    {loading ? <p role="status">Loading MCP servers…</p> : null}
    <form className="mcp-settings-form" onSubmit={save}>
      <fieldset disabled={loading || !loaded || saving}>
        <div className="provider-actions">
          <Button type="button" variant="outline" onClick={() => addExample('local-tools', { transport: 'stdio', enabled: false, command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', '/path/to/workspace'], env: {} })}>Add local example</Button>
          <Button type="button" variant="outline" onClick={() => addExample('remote-tools', { transport: 'streamable_http', enabled: false, url: 'https://example.com/mcp' })}>Add remote example</Button>
        </div>
        <label htmlFor="mcp-json">Server configuration (JSON)</label>
        <Textarea id="mcp-json" className="mcp-json" rows={18} spellCheck={false} autoComplete="off"
          aria-describedby="mcp-help" value={draft}
          onChange={event => { setDraft(event.target.value); setStatus(''); }} />
        <p id="mcp-help">Add named servers under <code>mcpServers</code>. Set <code>enabled</code> to false to pause a server, or remove its entry to delete it. An empty object disables MCP for this profile.</p>
        <details><summary>Connection options</summary>
          <p>Local servers use <code>transport: "stdio"</code>, a command, an args array, and optional env values. Commands run on the machine hosting Rynna, with its permissions.</p>
          <p>Remote servers use <code>transport: "streamable_http"</code> and a URL. For authentication, set <code>bearer_token_env</code> to an environment variable name available to Rynna.</p>
          <p>Examples start disabled. Edit their paths or URLs, then enable them. Subscription providers do not use external MCP tools. Saving validates configuration; servers connect when you send a message.</p>
        </details>
        <Button type="submit" disabled={!client.saveMcpSettings}>{saving ? 'Saving…' : 'Save MCP servers'}</Button>
      </fieldset>
    </form>
    {!loading && !loaded ? <Button type="button" onClick={() => setAttempt(v => v + 1)}>Retry loading MCP servers</Button> : null}
    {error ? <p role="alert">{error}</p> : null}
    {status ? <p role="status">{status}</p> : null}
  </>;
}
