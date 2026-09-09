import { type FormEvent, useEffect, useState } from 'react';
import type {
  AgentClient,
  HindsightDeployment,
  MemorySettings,
  MemorySettingsInput,
} from '../contracts';
import { Button } from './ui/button';
import { Input } from './ui/input';

const CLOUD_URL = 'https://api.hindsight.vectorize.io';
const SELF_HOSTED_URL = 'http://localhost:8888';

export function MemorySettingsPanel({
  client,
  profile,
}: {
  client: AgentClient;
  profile: string;
}) {
  const [saved, setSaved] = useState<MemorySettings>({ kind: 'none' });
  const [kind, setKind] = useState<'none' | 'hindsight'>('none');
  const [deployment, setDeployment] = useState<HindsightDeployment>('cloud');
  const [apiBase, setApiBase] = useState(CLOUD_URL);
  const [bankId, setBankId] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [clearKey, setClearKey] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadFailed, setLoadFailed] = useState(false);
  const [loadAttempt, setLoadAttempt] = useState(0);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState('');

  function apply(settings: MemorySettings) {
    setSaved(settings);
    setKind(settings.kind);
    if (settings.kind === 'hindsight') {
      setDeployment(settings.deployment);
      setApiBase(settings.api_base);
      setBankId(settings.bank_id);
    }
    setApiKey('');
    setClearKey(false);
  }

  useEffect(() => {
    let active = true;
    setLoading(true);
    setLoadFailed(false);
    setError(null);
    void client.getMemorySettings!(profile)
      .then((settings) => {
        if (active) {
          apply(settings);
          setLoading(false);
        }
      })
      .catch((reason: unknown) => {
        if (active) {
          setLoadFailed(true);
          setError(
            reason instanceof Error
              ? reason.message
              : 'Could not load memory settings.',
          );
          setLoading(false);
        }
      });
    return () => {
      active = false;
    };
  }, [client, profile, loadAttempt]);

  const hasSavedKey =
    saved.kind === 'hindsight' &&
    saved.api_key_configured &&
    saved.deployment === deployment &&
    saved.api_base.replace(/\/$/, '') === apiBase.replace(/\/$/, '');

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!client.saveMemorySettings || loading || saving) return;
    setSaving(true);
    setError(null);
    setStatus('');
    const settings: MemorySettingsInput =
      kind === 'none'
        ? { kind }
        : {
            kind,
            deployment,
            api_base: apiBase.trim(),
            bank_id: bankId.trim(),
            ...(clearKey ? { api_key: '' } : apiKey ? { api_key: apiKey } : {}),
          };
    try {
      apply(await client.saveMemorySettings(settings, profile));
      setLoadFailed(false);
      setStatus('Memory settings saved. Changes apply to your next request.');
    } catch (reason) {
      setError(
        reason instanceof Error
          ? reason.message
          : 'Could not save memory settings.',
      );
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
      <div className="settings-heading">
        <div>
          <h2>Memory provider</h2>
          <p>
            Remember useful context across this profile’s conversations. Memory
            is off by default.
          </p>
        </div>
      </div>
      {loading ? <p role="status">Loading memory settings…</p> : null}
      <form
        className="memory-settings-form"
        onSubmit={save}
        onChange={() => setStatus('')}
      >
        <fieldset disabled={loading || saving || !client.saveMemorySettings}>
          <label htmlFor="memory-provider">Memory provider</label>
          <select
            id="memory-provider"
            value={kind}
            onChange={(event) => {
              setKind(event.target.value as typeof kind);
              setApiKey('');
            }}
          >
            <option value="none">None</option>
            <option value="hindsight">Hindsight</option>
          </select>
          {kind === 'none' ? (
            <p>
              No conversations are sent to a memory provider. Previously stored
              memories are kept by that provider.
            </p>
          ) : (
            <>
              <label htmlFor="memory-deployment">Hosting</label>
              <select
                id="memory-deployment"
                value={deployment}
                onChange={(event) => {
                  const next = event.target.value as HindsightDeployment;
                  setDeployment(next);
                  setApiKey('');
                  setClearKey(false);
                  setApiBase(
                    saved.kind === 'hindsight' && saved.deployment === next
                      ? saved.api_base
                      : next === 'cloud'
                        ? CLOUD_URL
                        : SELF_HOSTED_URL,
                  );
                }}
              >
                <option value="cloud">Hindsight Cloud</option>
                <option value="self_hosted">Self-hosted</option>
              </select>
              <label htmlFor="memory-api-base">API URL</label>
              <Input
                id="memory-api-base"
                type="url"
                required
                readOnly={deployment === 'cloud'}
                value={apiBase}
                onChange={(event) => {
                  setApiBase(event.target.value);
                  setApiKey('');
                }}
              />
              <label htmlFor="memory-bank">Memory bank ID</label>
              <Input
                id="memory-bank"
                required
                maxLength={256}
                value={bankId}
                onChange={(event) => setBankId(event.target.value)}
              />
              <label htmlFor="memory-api-key">
                API key{deployment === 'self_hosted' ? ' (optional)' : ''}
              </label>
              <Input
                id="memory-api-key"
                type="password"
                autoComplete="new-password"
                value={apiKey}
                disabled={clearKey}
                required={deployment === 'cloud' && !hasSavedKey}
                aria-describedby="memory-key-help"
                onChange={(event) => setApiKey(event.target.value)}
              />
              <p id="memory-key-help">
                {hasSavedKey
                  ? 'A key is saved. Leave this blank to keep it.'
                  : deployment === 'cloud'
                    ? 'Enter your Hindsight Cloud API key.'
                    : 'Only needed if your server requires authentication.'}
              </p>
              {hasSavedKey && deployment === 'self_hosted' ? (
                <label className="memory-clear-key">
                  <input
                    type="checkbox"
                    checked={clearKey}
                    onChange={(event) => {
                      setClearKey(event.target.checked);
                      setApiKey('');
                    }}
                  />{' '}
                  Remove saved API key
                </label>
              ) : null}
              <p>
                This profile uses this bank. Use a different bank ID for each
                profile to keep memories separate. Rynna recalls relevant
                memories before responding and sends each completed user message
                and answer to Hindsight.
              </p>
            </>
          )}
          <Button type="submit">
            {saving ? 'Saving…' : 'Save memory settings'}
          </Button>
        </fieldset>
      </form>
      {loadFailed ? (
        <>
          <p>
            Settings could not be loaded. Retry or save replacement settings for
            this profile. If memory.yaml has invalid syntax, repair the file first
            to preserve other profiles.
          </p>
          <Button
            type="button"
            disabled={loading || saving}
            onClick={() => setLoadAttempt((attempt) => attempt + 1)}
          >
            Retry loading memory settings
          </Button>
        </>
      ) : null}
      {error ? <p role="alert">{error}</p> : null}
      {status ? <p role="status">{status}</p> : null}
    </>
  );
}
