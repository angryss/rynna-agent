# Subscription tool bridges

OpenAI account models advertise the `CompletionRequest.tools` supplied by Rynna core as app-server `thread/start.dynamicTools` function definitions (`type`, `name`, `description`, `inputSchema`). The adapter requires the experimental app-server API, not an exact CLI version. Protocol shapes can be inspected without reading credentials using `codex app-server generate-json-schema --experimental --out <temporary-directory>`.

## Authority

The adapter does not execute tools. It accepts only `item/tool/call` requests with a valid RPC ID, matching thread/turn IDs, a nonempty call ID, object arguments, no namespace, and an exactly advertised tool name. It converts one request per completion into a Rynna `ToolCall`. Core dispatches that call through its existing tool registry and policies; unavailable tools and tool errors do not grant new authority. Schema-specific argument validation remains the tool adapter's responsibility.

Codex runs in an ephemeral temporary working directory with read-only sandboxing, `approvalPolicy = never`, and its shell, image viewer, plan updater, and web search disabled. Native tool lifecycle items and unsupported server requests fail closed. The bridge never approves a Codex command or falls back to Codex shell execution. Profile permissions, disabled toolsets, command aliases, credential selection, and model enablement defaults are unchanged. Host commands remain explicitly opt-in as described in ADR 0003.

Before starting a thread, the adapter inventories the selected account's MCP servers with the read-only `codex mcp list --json` command and explicitly disables every returned name in the per-thread configuration. This includes runtime-added servers which may be absent from app-server `config/read`. An empty `mcp_servers` table does **not** clear inherited servers: Codex merges it. Disabled runtime-added entries also need a valid transport, so the adapter supplies inert stdio/HTTP transports rather than forwarding commands, URLs, headers, or environment values. Inventory output is bounded, never logged, and fails closed on malformed data or unsupported transports. No account configuration is modified. Rynna-advertised dynamic tools remain available and policy-controlled.

## Continuations and bounds

Each provider completion owns its own process group and ephemeral thread. On a dynamic call the completion returns immediately and the process group is dropped; the outstanding Codex RPC is deliberately not kept alive across core tool execution. The next completion injects the complete supplied history with native Responses API `function_call` and `function_call_output` items, preserving call IDs, arguments, result text (including denials), and order. It starts a continuation turn rather than repeating the original user request. A later user turn receives the same preceding history. This stateless replay avoids shared mutable session state and cross-conversation routing. It does not preserve private Codex reasoning state or execute speculative parallel calls after the first yielded request.

Malformed, orphaned, duplicate, or unfinished tool history is rejected before launching a process. Existing protocol message limits (1 MiB), response/reasoning limits (1 MiB each), message-count limits, operation deadlines, and process-group cleanup remain in effect. Core continues to enforce its model-turn, tool-call, aggregate result-size, and execution-time budgets. An incompatible app-server fails the request rather than silently removing tools or changing permissions.

## Regression coverage

The deterministic subprocess peer verifies dynamic-tool schemas and sandbox settings, emits protocol requests, and checks native result injection. Tests exercise provider round trips, streaming through the real core tool loop, tool-level denial, unavailable tools, malformed arguments/IDs, wrong-turn and namespaced calls, unsupported requests, malformed history, and existing tool-free/version-independent behavior. No account inference or credentials are required for these tests.

The opt-in live smoke test exercises both a no-tools OS question and an explicitly advertised host tool followed by native result replay with existing Rynna account authentication:

```sh
RYNNA_LIVE_CODEX_MODEL=<enabled-account-model> cargo test -p rynna-provider-openai \
  --features tokio/fs --test codex_live -- --ignored --nocapture
```

It makes real inference requests and reads only `/etc/os-release` for the explicitly requested host result. Normal test runs ignore it. The profile defaults to `default`; override with `RYNNA_LIVE_CODEX_PROFILE`. A profile with no opted-in host capability should answer that it cannot inspect the host, not use Codex's ambient MCP tools.

## Provider invariant and Claude subscription bridge

Every built-in adapter must accept the agent's advertised tools, independent of the model the user selects. OpenAI-compatible (including MLX/Ollama/OpenRouter), Anthropic API, Codex account, and Claude subscription selections therefore keep the same Rynna tool authority. Fallback chains retain their conservative all-members-support-tools check; all built-in members satisfy it. The chain does not drop definitions to accommodate a subscription member. Model-specific protocol failures remain errors, not permission escalation. Model enablement and command capability opt-in defaults are unchanged.

Claude subscription uses the real CLI `--json-schema` interface, with `result.structured_output` as the decision envelope. This is a structured-decision bridge, **not** Claude native tool execution or an MCP server. The pinned Claude Code 2.1.223 CLI's `--help` documents the flag; the [official structured-output contract](https://code.claude.com/docs/en/agent-sdk/structured-outputs) documents the result field and failure behavior. No authenticated inference is needed to inspect these interfaces. Offline tests verify our adapter contract; they do not establish live subscription/model behavior.

The adapter sends JSON containing the ordered conversation and currently advertised tool descriptions/input schemas on stdin. A trusted system instruction asks for either one Rynna tool decision or a final answer. Successful structured decisions become core `ToolCall` values; only core executes them. The following completion replays call IDs, arguments, and actual results (including denials) in the transcript. Claude's private reasoning state is not retained. Intermediate CLI prose is not surfaced as the answer; tool-enabled responses emit validated final content only. Tool-free conversations retain their existing text streaming.

Claude still runs in an ephemeral directory with `--safe-mode`, `--tools ""`, `--disallowedTools mcp__*`, `--no-chrome`, disabled slash commands, and no session persistence. Ambient hooks, plugins, shell/filesystem tools, and MCP configurations are not enabled. The internal `StructuredOutput` formatter is the only accepted native tool-use envelope in structured mode and is never dispatched to Rynna. Unknown tool names, nonobject arguments, empty IDs, multiple calls per decision, malformed histories, absent/invalid structured output, duplicate results, or native executable tool events fail closed. Tool-specific schema validation stays with the tool adapter. Existing prompt/message/output bounds, shared operation deadline, and process-group cleanup remain active; core keeps its tool/turn/result/time budgets.

Claude profiles may now explicitly declare capabilities, skills, and MCP servers, just like other providers. The removed catalog prohibition was a second independent blocker beyond the provider capability flag. CLI regression coverage exercises no active capability, opted-in native command execution, disabled file/command toolsets, and an API-to-Claude fallback with the same tool catalog. A separate regression asserts all built-in adapters preserve tool support in mixed fallback chains.
