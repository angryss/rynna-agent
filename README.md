# Rynna

Rynna is an open-source AI software agent built with Rust, React, and Tauri. One shared application core powers an interactive CLI, deterministic one-shot jobs, a long-running HTTP service, a browser UI, and a native desktop app.

> **Project status:** bootstrap foundation. The model-provider path, versioned profiles, native workspace filesystem and bounded command tools, profile-specific MCP tools, and all product surfaces are working. Skill execution, approvals, and long-running autonomous loops remain future capabilities.

## Why Rynna

- **Local by default:** connects to an OpenAI-compatible endpoint at local Ollama by default.
- **Automation friendly:** `rynna run` reads a flag or stdin and supports machine-readable JSON.
- **VPS ready:** `rynna serve` is stateless, handles graceful shutdown, and can serve the web build from the same binary.
- **One core, several surfaces:** HTTP, terminal, and Tauri code remain thin adapters around `rynna-core`.
- **Provider portable:** use Ollama locally, OpenRouter, or another OpenAI-compatible API.
- **Cache aware:** stable prompt prefixes are routed through a replaceable cache optimizer and translated to each provider's supported server-side cache controls.
- **Profile scoped:** local, work, automation, and hosted profiles can select different providers, models, system prompts, native capabilities, active skills, and MCP servers.

## Repository layout

```text
apps/
  cli/                 Rust CLI, one-shot runner, and HTTP server composition root
  desktop/             React/Vite frontend and Tauri host
  web/                 React/Vite web entrypoint and HTTP adapter
crates/
  rynna-config/      Versioned YAML profile catalog and validation
  rynna-core/        Domain types, model-provider port, and agent orchestration
  rynna-mcp/         Profile-specific stdio and Streamable HTTP MCP tools
  rynna-provider-anthropic/ Anthropic Messages API and Claude subscription adapters
  rynna-provider-openai/  OpenAI-compatible HTTP adapter
  rynna-server/      Axum API and static SPA hosting
  rynna-tools-filesystem/ Native workspace-scoped filesystem tool adapter
packages/
  ui/                  Shared React conversation UI and client contract
docs/
  adr/                 Architecture decisions
```

See [the architecture guide](docs/architecture.md) for dependency boundaries and extension points.

## Prerequisites

- Rust 1.88 or newer
- Node.js 22 or newer and npm
- [Ollama](https://ollama.com/) for the default local provider, or another OpenAI-compatible endpoint
- An [OpenRouter API key](https://openrouter.ai/keys) when using OpenRouter
- [OpenAI Codex CLI 0.149.1](https://developers.openai.com/codex/cli/) to configure OpenAI with a ChatGPT subscription or API key; the desktop account-backed chat provider rejects unreviewed Codex versions fail-closed
- [Claude Code 2.1.223](https://docs.anthropic.com/en/docs/claude-code) for Claude subscription / usage bundle profiles
- Tauri 2 platform prerequisites when building the desktop app

## Local quick start

Start Ollama and install the default model:

```bash
ollama serve
ollama pull qwen3:8b
```

In another terminal, start an interactive session:

```bash
cargo run -p rynna-cli -- chat
```

Thinking in the interactive terminal is capped at eight wrapped lines with a scrollbar, initially showing the latest lines. Use Ctrl-T to expand or collapse the latest thinking block and Alt-PgUp/Alt-PgDn to scroll its contents.

In the interactive terminal, type `/` to open command typeahead. Use the arrow keys to select a command, Tab to complete it, and Enter to run it. Available commands are `/clear`, `/help`, and `/quit`; `/exit` is an alias for `/quit`. Thinking-model reasoning streams into a dim section while it is active, collapses when the user-facing answer begins, and can be expanded or collapsed with Ctrl-T.

Choose an enabled provider and model during chat with `/model`; set effort with `/thinking default|low|medium|high`. Desktop and web chat provide the same choices above the composer. See [in-chat model selection](docs/model-selection.md) for commands, provider support, and request fields.

Run one unattended request:

```bash
cargo run -p rynna-cli -- run --prompt "Summarize this repository" --output json
printf 'Draft a release checklist' | cargo run -p rynna-cli -- run --output json
```

With a profile catalog, select a profile explicitly or list the available profiles without contacting a provider:

```bash
cargo run -p rynna-cli -- --config rynna.example.yaml profiles
cargo run -p rynna-cli -- --config rynna.example.yaml --profile local chat
cargo run -p rynna-cli -- --config rynna.example.yaml --profile work run --prompt "Review this change"
```

Select a named project for a new chat or run, or manage the selected profile's projects directly:

```bash
cargo run -p rynna-cli -- --config rynna.example.yaml --profile local --project rynna chat
cargo run -p rynna-cli -- --config rynna.example.yaml --profile local projects create product --directory /projects/app --directory /projects/api --default-directory /projects/app
cargo run -p rynna-cli -- --config rynna.example.yaml --profile local projects update product --default-directory /projects/api
cargo run -p rynna-cli -- --config rynna.example.yaml --profile local projects delete product
```

Open the terminal provider settings interface with:

```bash
cargo run -p rynna-cli -- --configure-providers
```

The TUI starts with an empty provider list and supports adding, editing, and deleting Ollama, MLX, OpenRouter, OpenAI, and Anthropic settings. Use `--provider-config <path>` to select a non-default provider settings file. OpenRouter reads `OPENROUTER_API_KEY` from the Rynna process environment. OpenAI can authenticate through an API key sent directly to Codex over stdin or through Codex's ChatGPT browser sign-in.

## Migrating configuration from TOML

Rynna reads and writes YAML application configuration. Default filenames are
`config.yaml`, `providers.yaml`, `mcp.yaml`, and `memory.yaml` in the same
platform configuration directory as before. MCP and memory files live beside
the provider settings file, including with a custom `--provider-config` path.
Explicit `--config` and `--provider-config` paths accept YAML content, including
`.yml` filenames. Cargo and rustfmt configuration keep their required TOML format.

For an existing installation, stop Rynna, back up the four TOML files privately,
and convert their contents to YAML before restarting. Renaming the extension
alone does not convert the contents. Keep the same keys, values, profile names,
and `version: 1`; represent TOML tables as nested mappings and arrays of tables
as YAML lists. Use [`rynna.example.yaml`](rynna.example.yaml) and the provider,
MCP, and memory examples below as references. Preserve strings as strings
(quote numeric-looking model names and other ambiguous scalar values).
Save the converted files under the new names with owner-only permissions
(`chmod 600` on Unix). Update custom config paths, environment variables, and
service/container mounts, then validate the catalog with `rynna doctor --config
/path/to/config.yaml`. Restart every Rynna process after converting all four files.

When a YAML file is absent but its same-name `.toml` predecessor exists, Rynna
reports a migration error instead of loading empty/default settings. Existing
YAML takes precedence; TOML backups are never modified or automatically imported.

Provider settings, for example, use lists under each profile:

```yaml
version: 1
profiles:
  local:
    - kind: ollama
      api_base: http://127.0.0.1:11434/v1
```

## Custom models and MLX

In web or desktop **Settings → Models**, choose a profile and provider, enter the
exact **Model name**, and click **Add model**. Names are not restricted to a
built-in list: Ollama tags such as `my-model:latest`, Hugging Face IDs, and local
model paths are accepted. New entries are enabled; use **Default** to make one
preferred, or disable older entries. Duplicate names for the same provider are
rejected. Restart Rynna to load saved changes into chat.

Thinking streams as it arrives, including when a profile has fallback models.
Answer text also streams immediately for requests without tools. Tool-enabled
requests buffer answer text per model attempt until the turn succeeds, so text
from failed attempts is discarded. Streaming requests try the next model only
if the current model fails before showing non-empty thinking or answer text;
failures after visible output begins are reported without switching models.

For MLX LM on macOS, start your model server, for example:

```bash
mlx_lm.server --model mlx-community/Qwen3.8-27B-8bit --host 127.0.0.1 --port 8000
```

MLX uses the existing OpenAI-compatible Chat Completions adapter without an API
key. New installations include an `mlx` provider at `http://127.0.0.1:8000/v1`
available in Settings → Models. If you already have a `config.yaml`, add this
connection definition and restart to expose it in the provider picker:

```yaml
providers:
  mlx:
    kind: mlx
    api_base: http://127.0.0.1:8000/v1
```

Then add `mlx-community/Qwen3.8-27B-8bit` under that provider in Settings, or
configure a profile directly:

```yaml
profiles:
  mlx:
    provider: mlx
    model: mlx-community/Qwen3.8-27B-8bit
```

Use any model name served by your MLX server; Rynna sends it unchanged. Start
that profile with `rynna --profile mlx chat`, or use `--model <name>` for a CLI
model override. See `rynna.example.yaml` for a complete example.

As with Ollama, the **Provider credentials** panel and terminal provider settings
record profile-scoped connection readiness only. Runtime routing uses
`providers` and `profiles` in `config.yaml`; changing a port in provider
settings does not change the runtime endpoint. Update `api_base` in the catalog
and restart when your server moves.

## Web application

For frontend hot reload, run the API and Vite separately:

```bash
cargo run -p rynna-cli -- serve
npm run dev
```

Open <http://127.0.0.1:5173>. Vite proxies API requests to port 3000. Select the active profile in the header. Click a project name in the Projects sidebar to start a new session in that project. New sessions and workflow selections remain unsaved drafts until a request is submitted. A successful or stopped first turn (or a started workflow) saves the session with a name derived from the opening submission. A separate, tool-free model call then generates a short title in the background using the selected profile/model, with a 15-second timeout and the original name as fallback. Naming runs once per new session and preserves manual `/title` names; the Projects sidebar groups sessions by project and restores their transcript and session ID when selected. The stateless `POST /v1/session-title` endpoint (desktop `session_title`) accepts `prompt` and optional `profile`/`selection`, returning a title string without recording a conversation. Session history stays in that browser or desktop webview and is not sent to profile configuration. Open **Settings** to add, edit, or delete profiles and profile-owned projects, arrange each profile's ordered provider/model fallback chain, and manage shared provider credentials from the left navigation. Press Enter in the composer to submit; use Shift-Enter or Alt-Enter to insert a newline. The browser streams typed thinking and content events from the server, keeps the active thinking section open, and collapses it when the user-facing answer begins. Select the Thinking summary to expand or collapse it manually. Expanded thinking is capped at eight visible lines with a scrollbar and follows the latest streamed text; scrolling up pauses following until you return to the bottom. While a response is processing, **Stop** replaces Send. Stop interrupts the active model request, delegated subagents, and owned command/MCP processes; partial answer text remains in the saved conversation. On Unix hosts, subscription CLI descendants are terminated as a process group. Stopping does not undo file changes or other actions already completed.

To exercise the production topology, build the SPA and serve it from the Rust process:

```bash
npm run web:build
cargo run -p rynna-cli -- serve --web-dir apps/web/dist
```

Open <http://127.0.0.1:3000>.

## Desktop application

```bash
npm install
npm run desktop:dev
```

The desktop frontend uses narrow Tauri commands and a typed IPC channel instead of opening the HTTP server. Its shared **Settings** navigation provides the same profile editor and blank-by-default provider-credential CRUD as the browser and CLI. Its shared composer submits with Enter and inserts newlines with Shift-Enter or Alt-Enter. It provides the same streaming, collapsible thinking display as the browser, loads the same configured profile catalog as the CLI plus the reserved `openai-account` desktop profile, and exposes profile metadata and selection through the shared UI contract.

Web, CLI, and desktop profiles can use the OpenAI account connected under **Settings → Provider credentials**. Saving an OpenAI connection registers the `openai-account` provider and a disabled `Codex default` model in that profile's **Models** section. Previously saved connections are registered at startup without another login. Select `openai-account`, select **Codex default**, and click **Enable selected** (or add a specific model ID available to your account). Optionally make it the default, then restart Rynna before selecting it in chat. `Codex default` lets Codex choose its model; an explicit ID is passed unchanged to Codex. This path uses the saved account's ChatGPT subscription or API-key authentication, not the OpenAI-compatible HTTP adapter.

The **Model name** field looks up models from the selected provider and filters suggestions as you type. Use the arrow keys and Enter or click a suggestion, then choose **Add model**. You can also enter any custom model ID, including one absent from the list. Lookup failures and providers without discovery support leave manual entry available. OpenAI account suggestions come from Codex’s read-only `model/list` request using the selected profile’s credentials; OpenAI-compatible providers (including Ollama, MLX, and OpenRouter) and the Anthropic API use their model-list endpoints. Claude subscription profiles support manual entry. Model discovery does not start an inference turn.

Account models are initially disabled to preserve existing defaults and tool-enabled fallback chains. They do not receive Rynna tools, skills, MCP tools, or delegated subagents. A fallback chain containing an enabled account model is also tool-free; choose a tool-capable model explicitly or keep account models in a separate profile when tools are needed. Credentials remain profile-scoped settings: newly signed-in accounts use Rynna's private Codex directory, while **reuse existing credentials** uses the ordinary Codex account. `RYNNA_CODEX_PATH` and `RYNNA_CODEX_HOME` apply to the CLI/server as well as desktop. Deleting the profile's OpenAI credential entry prevents subsequent account-model requests even before restart.

The desktop app also exposes **Connect OpenAI**. Choose **Use ChatGPT subscription** to complete Codex's supported browser sign-in, or enter an OpenAI API key for usage-based API billing. When adding a ChatGPT-backed OpenAI provider later, Rynna checks the user's existing Codex account and asks whether to reuse those ChatGPT credentials or complete a new browser sign-in in Rynna's private Codex configuration directory. Rynna verifies reused credentials with `codex login status`, passes API keys to Codex over stdin, and never returns credentials through Tauri IPC. After connecting, select the `openai-account` profile to send prompts through that account. This account-backed profile does not receive Rynna tools. Its ephemeral Codex thread has no execution environment; shell, image, planning, and web-search tools are disabled, any tool lifecycle item aborts the response, and the model is instructed to answer only from the supplied conversation. The provider is pinned to the reviewed `codex-cli 0.149.1` protocol/tool surface; upgrading Codex requires an Rynna compatibility review and release.

## Indexed code search

Filesystem capabilities include `code_search` by default. It lazily builds a private, persistent SQLite index for each repository, reuses those indexes across sessions, updates changed files on subsequent searches, and returns short path/line snippets instead of whole files. The model is instructed through tool descriptions to prefer it for literal code searches; `search_files` remains available for targeted regex or one/two-character searches.

Multi-repository project sessions search their selected indexes together under one output budget; each match identifies its repository. Indexing handles repositories beyond the ordinary filesystem traversal and aggregate-read limits. It streams files to disk, honors nested `.gitignore` and `.ignore` files, and never follows source symlinks. Long builds return `status: indexing` while continuing in the background; repeat the query to retrieve the result. MCP plugins can replace the built-in search tool by selecting their remote `code_search_tool` in a profile's MCP settings.

See [code search configuration, scale measurements, and limits](docs/code-search.md).

## Conversation context and compaction

CLI chat (terminal and plain text), web, and desktop support `/compact`. It asks the selected model to summarize previous conversation context while preserving the transcript. Summaries travel with the assistant message, survive saved-session reloads and model changes, and remain untrusted user-level reference data. Compaction does not execute tools or retain an extra memory exchange. Failed compaction leaves history intact.

The shared agent automatically summarizes at 75% of its input context allowance, including system instructions, history, recalled memory, and discovered tool definitions/results. Long histories are summarized in bounded chunks; the latest request is preserved verbatim. The same check runs between tool turns. If the latest request and fixed instructions/tools cannot fit, the request fails with a context-limit error instead of silently truncating it. Workflow runs retain their stricter policy of rejecting oversized context to preserve acceptance criteria.

The composer/terminal shows estimated usage as a percentage. Web/desktop also include the unsent draft, with token counts and the limit in the indicator tooltip. Counts use a byte-based estimate, not a provider tokenizer; dynamic tools and memory added during execution can increase actual usage. Automatic compaction includes these at execution time. During an active response the web/desktop indicator retains the estimate from before that response, then refreshes on completion.

Set each model's **Context window (tokens)** in Settings → Models, or add `context_window: 32768` to its profile provider entry in `config.yaml`. Use the actual serving limit, especially for local models. Saved model settings apply after restart. Explicit limits override built-in defaults for GPT-5.2 (400,000) and Claude Sonnet/Haiku 4.5 (200,000), including their listed dated IDs. These defaults follow [OpenAI's model documentation](https://developers.openai.com/api/docs/models/gpt-5.2) and [Anthropic's context documentation](https://platform.claude.com/docs/en/build-with-claude/context-windows). Other IDs/endpoints use an 8,192-token fallback, shown as **budget** rather than a known model context limit. Profile-default routing uses the smallest enabled fallback allowance; explicitly selecting a model uses that model's allowance.

`POST /v1/context` and desktop `conversation_context` accept `profile`, `project`, `selection`, `session_id`, `history`, optional `prompt`, and `compact` (default false). They return `history`, `size: { current_tokens, max_tokens }`, `compacted`, and `limit_known`. Estimation does not call a model. Clients must preserve returned message metadata when resending history. Manual compaction has a 60-second overall deadline.

## Web and desktop slash commands

Type `/` at the start of the chat composer to browse commands. Keep typing to filter,
use ↑/↓ to select, Tab to complete, Enter to run, or click a command. Escape dismisses
the menu without changing the draft; Shift+Enter and Alt+Enter still insert newlines.

| Command | Action |
| --- | --- |
| `/new`, `/clear` | Start a fresh chat in the current project, keeping saved sessions. |
| `/compact` | Summarize the active context while preserving the visible transcript. |
| `/retry` | Resend the last user message with its preceding history, replacing the last exchange. |
| `/title <name>` | Rename the current saved chat. |
| `/save` | Export the visible user/assistant transcript as JSON. |
| `/model` | Open the existing model and thinking-level picker. |
| `/settings` | Open Settings for the current connection. |
| `/help` | Show the command menu. |

Desktop exports go to the system Downloads directory.

Commands are handled by the shared UI. Unknown commands and invalid arguments show
an error instead of being sent to the model. Commands are unavailable while a response
or autonomous workflow is running. `/retry` sends a model request and can repeat tool actions.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `RYNNA_CONFIG` | platform config path | Explicit profile-catalog path; equivalent to `--config` |
| `RYNNA_PROVIDER_CONFIG` | `<config-dir>/rynna/providers.yaml` | Provider settings path; equivalent to `--provider-config` |
| `RYNNA_PROFILE` | catalog `default_profile` | Process default profile; equivalent to `--profile` |
| `RYNNA_PROJECT` | default project | Named project for `chat` and `run`; equivalent to `--project` |
| `RYNNA_API_BASE` | `http://127.0.0.1:11434/v1` | OpenAI-compatible API base URL |
| `RYNNA_MODEL` | `qwen3:8b` | Provider model identifier |
| `RYNNA_API_KEY` | unset | Optional bearer token; never place it in source control |
| `OPENROUTER_API_KEY` | unset | OpenRouter bearer token referenced by the example OpenRouter provider |
| `ANTHROPIC_API_KEY` | unset | Example direct Messages API credential referenced by an Anthropic provider's `api_key_env` |
| `RYNNA_CODEX_PATH` | `codex` on `PATH` | Codex CLI executable used by desktop OpenAI account support |
| `RYNNA_CODEX_HOME` | `<config-dir>/rynna/codex` | Private Codex credential/config directory owned by Rynna desktop |
| `RYNNA_CLAUDE_PATH` | `claude` on `PATH` | Claude Code executable used by CLI/web/desktop provider sign-in; profile execution uses `providers.<name>.claude_program` |
| `RYNNA_SYSTEM_PROMPT` | Rynna's built-in policy | Trusted instruction prepended by the core |
| `RUST_LOG` | `warn` | Rust tracing filter, such as `rynna=info` |
| `VITE_RYNNA_API_URL` | same origin | Optional API origin when an external reverse proxy supplies an appropriate CORS policy |

Copy `.env.example` as a reference, but load secrets through your shell, service manager, or secret store. Rynna does not automatically read `.env` files. CLI flags and the legacy provider environment variables override only the selected default profile, in this order: explicit flag/environment override, selected profile, built-in local Ollama default.

When `RYNNA_API_KEY` is set, Rynna requires HTTPS except for loopback development endpoints (`localhost`, `127.0.0.1`, and `::1`). Unsupported URL schemes and provider URLs containing embedded credentials are rejected. Interactive terminal responses use OpenAI-compatible SSE streaming so output appears incrementally while the composer remains edimapping. Provider response bodies are capped at 1 MiB.

Provider requests use server-side prompt caches without storing prompt content locally. Anthropic Messages requests enable automatic five-minute ephemeral caching. Requests to the official OpenAI API include a stable SHA-256 `prompt_cache_key` derived from the system prompt, first conversation message, and ordered tool definitions; this keeps routing stable as one conversation grows while separating conversations with different initial anchors. Byte-identical conversation prefixes intentionally share a routing scope because Rynna's stateless request model has no conversation identifier. OpenAI still determines cache eligibility and lifetime. Ollama requests retain stable system/tool/message ordering and rely on Ollama's automatic in-memory prefix cache, while avoiding OpenAI-only cache fields that Ollama's compatibility API does not support. Claude subscription requests delegate caching to the pinned Claude Code client. The `CacheOptimizer` core port can be replaced when another agent or cache technology needs a different scope or policy.

### Profile catalog

Rynna reads YAML from the platform configuration directory at `<config-dir>/rynna/config.yaml`. On macOS this is under `~/Library/Application Support`; on Linux it normally follows `XDG_CONFIG_HOME` or `~/.config`; on Windows it uses the roaming application-data directory. If the file does not exist, Rynna creates no files and uses the previous built-in `default` profile backed by local Ollama.

See [`rynna.example.yaml`](rynna.example.yaml) for the complete version 1 schema. The catalog separates reusable provider connections from profiles:

- `providers.<name>` may use `openai-compatible`, `anthropic-messages`, or `claude-subscription`. OpenRouter uses the OpenAI-compatible adapter at `https://openrouter.ai/api/v1` with `api_key_env: "OPENROUTER_API_KEY"`. Direct Anthropic profiles use `api_key_env` (normally `ANTHROPIC_API_KEY`); store secrets only in environment variables, never in YAML.
- `claude-subscription` uses Claude Code's supported headless interface after `claude auth login --claudeai` (or an explicit `CLAUDE_CODE_OAUTH_TOKEN` created by `claude setup-token`). Claude subscription / usage bundle billing is handled by Claude. Rynna removes competing API, profile, gateway, and cloud-provider environment overrides; disables Claude Code tools, MCP, customizations, and persistence; and rejects profiles that declare Rynna capabilities, skills, or MCP servers.
- Provider settings in the CLI, web app, and desktop app store shared credential readiness only. Runtime provider, model, and profile routing remains authoritative in `config.yaml` and is loaded at process startup.
- `profiles.<name>` selects an ordered, non-empty list of provider/model entries and may define `system_prompt`, `capabilities`, `active_skills`, `mcp_servers`, `default_project_directory`, and `projects`. The first provider entry is primary; later entries are attempted as fallbacks.
- `default_project_directory` is the starting directory for new sessions that do not select a named project. Each named project contains one or more directory paths and one `default_directory` chosen from that list. Projects are isolated per profile. They provide trusted model context but do not broaden native filesystem roots or command allowlists.
- `active_skills` enables standard `SKILL.md` packages independently for each profile. Add names or directories in Settings → Profiles → Skills or in the catalog. See [Agent Skills setup](docs/skills.md) for discovery paths, examples, permissions, and restart behavior.
- `capabilities.<name>` defines an in-process native capability. `kind: "filesystem"` supplies eight workspace-scoped tools: read, write, exact edit, list, find, search, create directory, and file metadata. `kind: "command"` supplies one bounded `run_command` tool over an explicit alias-to-executable map.
- `mcp_servers.<name>` stores a structured MCP server definition. Every profile reference is validated when the catalog loads.
- `default_profile` selects the profile used when a request or process does not specify one.

Native filesystem capabilities execute today through a provider-neutral tool loop bounded to eight model turns and 64 total tool calls. Filesystem roots are explicit capability handles; path traversal is descriptor-relative and no-follow, so tool paths reject all symlinks as well as absolute and parent-traversal paths. Metadata-only operations use descriptor-relative no-follow metadata without opening file content. Content handles are opened nonblocking where the platform supports it and validated as regular files before I/O; special files are rejected or omitted from listings and traversal. Secret patterns are denied by default, and `.git` is write-protected by default. Allow globs authorize final files and visible listing results, while nonmatching policy-safe parent directories may be traversed to reach matches; `create_directory` requires its final directory path to match the allowlist. Per-file reads, result counts, total visited directory entries, traversal depth, and bytes actually read for search are bounded; writes can use bounded SHA-256 optimistic concurrency checks. `read_only`, allow/deny/protected globs, and limits are profile-catalog settings. These controls do not replace an OS sandbox for hostile or multi-tenant workloads.

On macOS, mapped executables on the read-only root filesystem run from their canonical system paths instead of private copies: copied platform executables such as `uname` and `sw_vers` can be terminated by macOS. Rynna checks the opened file's read-only/root mount flags and verifies that the resolved path still identifies the same device and inode. Writable configured symlinks are resolved before execution, while executables outside that read-only root retain the private-copy behavior. The executable size limit and all command policies still apply.

Command capabilities are absent by default and currently supported only on Unix hosts. Each model-visible alias maps to one absolute executable path. At profile composition Rynna opens authority-bearing paths nonblocking, validates the retained objects, normally copies at most 64 MiB from the executable handle into a private execution directory, and retains an open handle to the configured working directory. Configured-source pathname replacement after composition by actors outside Rynna's OS identity therefore does not substitute different authorized objects. Root and every process running as Rynna's UID are trusted and outside this application boundary: they can tamper with Rynna's private snapshot or process state. Restart Rynna to pick up an executable update. `run_command` starts the prepared executable directly rather than invoking a shell, clears the inherited environment, uses null stdin, accepts at most 128 arguments and 32 KiB of argument text, and returns structured status and UTF-8 output. Each call is capped at 300 seconds and 1 MiB of combined stdout/stderr, with configured limits allowed only at or below those hard maxima. Output work is reserved before reads and is limited to the configured byte count plus one overflow-detection byte. Every invocation runs in a new process group; on timeout, output overflow, core cancellation, or future drop, an independent supervisor sends `SIGKILL` to that group and waits for Rynna's direct child. Orphaned descendants are reaped by the operating system. Only descendants that remain in the invocation's process group are signaled; a mapped executable can deliberately call `setpgid` or `setsid` and escape this mechanism, so only trusted executables should be mapped and hostile workloads require an OS sandbox. Cleanup failures are surfaced in the tool error and process diagnostics. The core additionally caps each response at 64 tool calls, five minutes of aggregate tool-loop time, and 8 MiB of serialized tool results.

For example, a macOS profile can map `uname` to `/usr/bin/uname` and `sw_vers` to `/usr/bin/sw_vers`, allowing prompts such as “What operating system is installed on this computer?” without granting an implicit shell. Mapping a shell, interpreter, package manager, or similarly powerful executable intentionally grants the model the authority of that program and its arguments. An executable allowlist and process group are not an OS sandbox: mapped programs retain every permission of the Rynna process. Run Rynna as a dedicated restricted OS user or inside a container with narrow mounts and network policy for untrusted or multi-tenant workloads.

Skills load through the profile-scoped `read_skill` tool; legacy catalog MCP names remain metadata. Executable MCP servers are configured independently for each profile in Settings → MCP servers (see below); listing a legacy activation name does not execute it.

## Profile-specific subagents

In **Settings → Subagents**, select a profile and add helpers with a name, a description of when to use them, and instructions. Each profile owns an independent list; the same helper name can have different instructions in different profiles. Renaming or deleting a profile carries or removes its saved list.

The parent model can call `delegate_task` with `subagent` and `task`. Each call runs a fresh helper conversation and returns its final answer to the parent. Helpers inherit the current model selection, profile policy and skills, selected project context, and permitted native/MCP tools. Include necessary context in the delegated task: earlier chat history and recalled memory are not copied, and helper exchanges are not retained to memory. Helpers share tool access, so they can modify the same permitted files as the parent.

Delegation is one level deep and sequential; helpers cannot delegate to other helpers. Each helper uses the existing bounded model loop. Parent and helpers share one 64-call budget and one 8 MiB tool-result byte budget per response, and the parent's 300-second aggregate deadline covers delegated work. Providers that disable external tools, including subscription adapters, do not expose delegation.

Subagents are saved inline in `config.yaml` and are available across CLI, HTTP and desktop. Public HTTP profile lists redact helper instructions; loopback administrators receive the complete definitions for editing:

```yaml
profiles:
  local:
    subagents:
    - name: reviewer
      description: Review code changes for correctness and missing tests.
      instructions: Inspect the requested changes. Report actionable issues with file references and suggested fixes.
```

Names must be unique within a profile and contain 1–64 ASCII letters, digits, underscores or hyphens. A profile supports up to 32 helpers; descriptions and instructions are required and limited to 1024 and 32000 UTF-8 bytes. Delegated tasks are limited to 32000 bytes. Omitted lists default to empty. Settings saves apply on the next request for an already-running profile; manual file edits and newly created or renamed profiles follow the existing restart lifecycle.

## HTTP API

`GET /v1/profiles` returns the process default, safe catalog provider identifiers, and safe profile metadata. It never returns API keys, API-key environment-variable names, provider base URLs, system prompts, or MCP command definitions. `POST /v1/profiles` and `PUT`/`DELETE /v1/profiles/{name}` add, update, and delete catalog profiles for loopback clients. Profile mutations persist to `config.yaml`, but they never attach an existing runtime agent to changed metadata: metadata for currently running profiles remains the startup snapshot, while new catalog-only profiles have no runtime agent. Restart the process before using a new or renamed profile or relying on changed providers, models, prompts, skills, or capabilities.

`GET /v1/providers`, `POST /v1/providers`, and `PUT`/`DELETE /v1/providers/{kind}` provide provider settings CRUD for the browser. The persisted YAML contains only Ollama's API base URL, the selected OpenAI/Anthropic authentication method, or an OpenRouter credential-readiness marker. OpenRouter reads its API key from `OPENROUTER_API_KEY`; OpenAI API keys are piped to Codex. Neither key is stored in this file or returned by the API.

`POST /v1/respond` accepts caller-owned user/assistant history, an optional profile name, an optional named project, and a new prompt. Omit `profile` to use the process default; omit `project` to use that profile's implicit default project:

```json
{
  "profile": "work",
  "project": "rynna",
  "prompt": "Continue the investigation",
  "history": [
    { "role": "user", "content": "Inspect the logs" },
    { "role": "assistant", "content": "I found a timeout" }
  ]
}
```

The response is:

```json
{
  "message": { "role": "assistant", "content": "..." }
}
```

`POST /v1/respond/stream` accepts the same request and returns `text/event-stream`. Each data event is JSON with `kind` set to `thinking` or `content` and a `content` string. The final event has `kind: "done"` and the complete assistant `message`; failures after streaming starts use `kind: "error"` with a safe `message`.

`GET /healthz` reports process readiness. The initial API is stateless: callers send history on each request.

## VPS deployment

The server binds to `127.0.0.1:3000` by default. Keep that default and expose Rynna through an authenticated TLS reverse proxy, VPN, or private network. Rynna does **not** yet provide public-edge authentication, rate limiting, or load shedding, so configure those controls at the proxy for shared deployments. Administrative provider endpoints additionally require the direct TCP peer to be loopback; direct non-loopback requests are rejected. A same-host authenticated TLS reverse proxy can therefore administer providers, while a remotely bound Rynna listener cannot expose those operations directly. This requirement is especially important because provider operations modify local settings and OpenAI API keys transit the authenticated request to Codex. Never expose them over unauthenticated or plaintext transport. The built-in server is same-origin by default and does not enable CORS; configure that explicitly at a trusted reverse proxy if the web UI and API use different origins.

The Compose configuration publishes only to host loopback by default and restarts the stateless service automatically. Its default provider URL is `http://host.docker.internal:11434/v1`; Docker Desktop provides that host name, while Compose maps it through Docker's `host-gateway` on Linux.

On a Linux host, Ollama's default loopback-only listener is not reachable from a bridge-networked container. Start Ollama so it listens beyond loopback before starting Rynna (or set the same `OLLAMA_HOST` value in the Ollama systemd service):

```bash
OLLAMA_HOST=0.0.0.0:11434 ollama serve
```

Keep TCP port `11434` firewalled from public ingress; it should be reachable only from Docker/private host networks. In another terminal, ensure the model is installed and then start Rynna:

```bash
ollama pull qwen3:8b
docker compose up --build -d
```

Remote OpenAI-compatible providers remain supported by setting `RYNNA_API_BASE`, `RYNNA_MODEL`, and, when required, `RYNNA_API_KEY` in the deployment environment. For multiple profiles, mount a catalog read-only, set `RYNNA_CONFIG` to its in-container or server path, and supply every referenced `api_key_env` through the deployment secret store. For a native deployment, adapt [`deploy/rynna.service`](deploy/rynna.service).

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
npm run check
npm test
npm run build
npm audit --audit-level=high
```

Install `cargo-audit` once with `cargo install cargo-audit --locked` before running the Rust dependency audit locally.

Behavior changes follow test-driven development. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Rynna is available under the [MIT License](LICENSE).

## Memory providers

Open **Settings → Memory provider** in web or desktop and choose a profile. Each profile defaults to **None**; existing installations make no memory calls until configured. Select **Hindsight** to reveal its settings:

- **Hindsight Cloud:** the official API URL, your memory bank ID, and an API key.
- **Self-hosted:** your server API base URL (for example `http://localhost:8888`), bank ID, and an optional API key for authenticated servers. Reverse-proxy path prefixes are supported; enter the base URL without `/v1/default/banks`.

Save to apply the choice to subsequent requests for that profile immediately. Provider choice, hosting, endpoint, bank ID, and credentials are independent per profile. Use distinct bank IDs to keep memories separate; pointing two profiles at the same bank intentionally shares its contents. Rynna recalls relevant context before each turn and queues the successful user message and final answer for background retention, including streaming responses. Answers do not wait for the memory write. Each conversation has a random session ID; web/desktop persist named sessions and their caller-visible transcript locally, terminal chat resets its session on `/clear`, and each one-shot run starts a new session. Selecting a saved web/desktop session after a page reload restores its session ID and transcript. Hover over a session to reveal its three-dot menu, choose Delete, and confirm Delete to remove its local history; deleting the active session starts a fresh chat in the same project. Deletions synchronize across windows sharing that local store. Finish or cancel nonterminal workflows before deleting their sessions. Deletion does not remove retained provider memories or host workflow records.

Following [Hermes Agent's Hindsight integration](https://github.com/NousResearch/hermes-agent/blob/main/plugins/memory/hindsight/__init__.py), Hindsight 0.5.0 and newer receive structured JSON messages with roles, content, and UTC turn timestamps, appended to a `rynna-<session UUID>` document. Each item carries the session ID, source, message count, turn index, and session tag. Older servers—or servers whose version cannot be determined—receive the complete caller-supplied transcript in a document unique to that provider instance and session. This compatibility path includes previous user/assistant history; historical timestamps that Rynna does not know are omitted. Tool output, thinking, recalled data, and internal compaction state are excluded.

Writes run in order per provider snapshot with a ten-second deadline per write. The process queue accepts up to 128 pending writes and up to 1 MiB of conversation text per write; overflow and failures are logged without failing chat. There is no automatic retry after an ambiguous network failure, which avoids duplicating appended turns. Normal CLI/server/desktop shutdown allows ten seconds for accepted writes to drain. The queue is in memory, so forced termination can lose pending writes. Hindsight processes accepted writes asynchronously; they may not be immediately searchable. Recalled text remains bounded and labeled as untrusted reference data.

Selecting **None** stops future memory operations for the selected profile and removes only its saved local credential. Existing memories remain on Hindsight; requests already in progress finish with their original provider. An unchanged blank API-key field preserves that profile’s saved credential only for the same endpoint and hosting mode. Self-hosted settings also offer an explicit remove-key checkbox.

Settings are stored as versioned `profiles.<name>` entries in `memory.yaml` beside `providers.yaml`, including when `--provider-config` / `RYNNA_PROVIDER_CONFIG` chooses a custom directory. CLI chat and one-shot runs read this file at startup. Web/desktop changes affect their current process; restart other running processes to pick up changes. Renaming a profile moves its memory settings; deleting a profile removes them locally without deleting remote memories. New or renamed profiles still become runnable after restart, consistent with the profile catalog. The file is written atomically with owner-only permissions on Unix. API keys are never returned to the frontend or written to browser storage.

For CLI-only configuration, create `memory.yaml` in that directory (restrict its permissions to your user):

```yaml
version: 1
profiles:
  local:
    kind: hindsight
    deployment: self_hosted
    api_base: http://localhost:8888
    bank_id: rynna
```

For Cloud, use `deployment: "cloud"`, `api_base: "https://api.hindsight.vectorize.io"`, and an `api_key`. Replace `local` with your profile name. To disable its memory, replace that profile’s mapping with `kind: "none"` or remove the mapping, leaving other profiles intact. The unreleased global format is rejected with migration instructions: add `version: 1` and place the previous settings under the intended `profiles.<name>` mapping.

The HTTP settings contract is `GET` / `PUT /v1/profiles/{profile}/memory`; both methods use the existing loopback-only administration restriction. Desktop uses `get_memory_settings` / `save_memory_settings` IPC commands, both with a required `profile` argument. Unknown profiles are rejected. The response APIs accept an optional `session_id` UUID. API clients should reuse it for a conversation and generate a new one for a new conversation; omission creates a fresh document per request. The provider-neutral `rynna_core::MemoryProvider` trait exposes `recall` and `retain(&MemoryConversation)`; implement this port and register a configuration variant and composition adapter to add another provider. Hindsight-specific HTTP behavior lives in `rynna-memory-hindsight`.

## Profile-specific MCP servers

Open **Settings → MCP servers**, choose a profile, and edit its JSON configuration.
Local **stdio** and remote **Streamable HTTP** servers expose tools to that profile’s
agent. Save validates the configuration without connecting or starting a command;
the next request connects, discovers tools, and makes them available to the model.
Each new profile starts empty. The same server name can have entirely different
configuration in different profiles. Rename moves settings; deletion removes them.

```json
{
  "mcpServers": {
    "workspace": {
      "transport": "stdio",
      "enabled": true,
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/workspace"],
      "env": {}
    },
    "remote": {
      "transport": "streamable_http",
      "enabled": true,
      "url": "https://example.com/mcp",
      "bearer_token_env": "MY_MCP_TOKEN"
    }
  }
}
```

Set `enabled` to `false` to pause a server, delete an entry to remove it, or save
`{"mcpServers": {}}` to disable all MCP tools for the selected profile. HTTP bearer
authentication reads the named environment variable from the Rynna process.
OAuth sign-in and legacy HTTP+SSE transport are not implemented.

Settings are persisted atomically in owner-only `mcp.yaml`, beside `providers.yaml`:

```yaml
version: 1
profiles:
  work:
    mcpServers:
      workspace:
        transport: stdio
        enabled: true
        command: npx
        args:
        - -y
        - '@modelcontextprotocol/server-filesystem'
        - /path/to/workspace
```

CLI reads this file at startup (`--provider-config` also selects the sibling MCP
file). HTTP and desktop UI saves apply to subsequent requests without restarting;
in-flight requests keep their original settings. New or renamed profiles still
require restart to become runnable, as with other profile configuration.
The old `config.yaml` global `mcp_servers` declarations and activation names remain
legacy metadata and are never executed or implicitly inherited. Copy the desired
configuration into each profile’s MCP editor to enable it explicitly.

The loopback-only HTTP administration endpoints are `GET` and `PUT`
`/v1/profiles/{profile}/mcp`; desktop exposes `get_mcp_settings` and
`save_mcp_settings` with a required `profile`. Commands and environment values are
visible only through this settings editor/API, not the public profile list.

Connections and tool discovery have a ten-second deadline per server. Tool calls
have a sixty-second deadline and a 1 MiB result limit, in addition to the agent’s
existing aggregate limits. Discovery failures identify the server and fail that
request; fix or disable the server to continue. Sessions are scoped to one response
and close on completion or cancellation. Server-returned content and `isError`
remain tool output. Subscription-backed providers do not connect to MCP servers.

Local MCP programs run on the Rynna host with its OS permissions, outside the native
filesystem/command capability restrictions. Rynna passes a small environment for
executable resolution plus explicitly configured `env` values; model-provider
credentials are not automatically inherited. Only configure programs and remote
servers you trust. `env` values are stored in the private file and are visible in
the editor; do not put that file in version control or serve it as a web asset.

## Autonomous workflows

Choose a workflow in a conversation, enter a goal, individual success criteria and finite limits, then select **Start workflow**. Existing conversations default to ordinary Chat. Settings → Workflows manages definitions in the selected profile; duplicate the read-only Rynna default to customize its plan → execute → verify process. Definitions contain 2–16 ordered steps, exactly one final verifier, and a repeat target pointing to an earlier work step. A step uses direct instructions or a named same-profile helper. IDs use 1–64 ASCII letters, digits, underscores or hyphens; profiles allow 32 custom workflows, descriptions allow 1024 bytes and instructions allow 32000 bytes. Revisions are assigned on save. Runs capture definitions and helpers so later edits affect future runs.

Default and maximum limits are 50 step executions, 512 total tool calls (including delegated calls), and 1800 active seconds. Inner response limits still apply. Tool allowance and up to 300 active seconds are reserved before each attempt; a successful checkpoint refunds unused resources. Interrupted or failed attempts retain their reservation. Workflows require providers with externally managed, countable tools; subscription providers that run their own tools are rejected. Each step receives the goal, all criteria, steering and bounded prior results. Oversized contexts fail rather than silently dropping criteria.

Verification must return JSON containing `results` (one per criterion, with `criterion_id`, `verdict`: `met|unmet|unknown`, `kind`: `test|artifact|qualitative`, `reference`, and a non-empty `excerpt`), `summary`, and `can_continue`. All criteria must be met with evidence to complete. Otherwise the runner repeats at the configured target, or blocks when input is missing or verification is malformed. Model judgments can be mistaken; inspect the recorded evidence.

**Pause** stops new dispatch and lets the current bounded step settle. **Stop** cancels the workflow immediately, dropping the active step and its delegated work; interrupted steps retain their reserved resource budget. Steering and optional criteria amendments are recorded after pausing; resume requires fresh verification. Profile/project destructive changes conflict with nonterminal runs. Ordinary chat conflicts while that session is running or stopping; paused conversations may discuss without implicitly steering the run.

The HTTP and desktop hosts use private, atomic, versioned files in `rynna/workflow-runs` beneath the configuration directory. Set `RYNNA_WORKFLOW_STORE` to an explicit private directory to select another store. Only one process may own a store. Browser disconnects do not stop host execution. Desktop exit stops its host. Restart exposes interrupted runs as paused, and uncertain steps require acknowledgement because retry may repeat side effects. Budgets persist across restart. Corrupt/unknown record versions make workflow storage unavailable until repaired; files remain intact and ordinary chat remains available. No credentials, raw thinking or internal tool transcripts are stored by the runner; goal, visible output and evidence can still contain private project data.

Stateful routes (under the same deployment trust boundary as conversation content):

- `GET /v1/profiles/{profile}/workflows`: public selection metadata, without step instructions.
- `GET /v1/profiles/{profile}/workflows/{id}`, `POST /v1/profiles/{profile}/workflows`, `DELETE /v1/profiles/{profile}/workflows/{id}`: loopback-only definition administration. POST creates or updates using the current revision.
- `POST /v1/workflow-runs`: start with UUID `request_id` and `session_id`, `profile`, optional `project`, explicit model `selection`, `workflow_id`, `goal`, `criteria`, `limits`, and optional `initial_context`. Retries return the original run.
- `GET /v1/workflow-runs?profile=...&session_id=...`: latest authoritative run for the conversation, bounded to one record. Historical runs remain readable at `GET /v1/workflow-runs/{id}?profile=...&session_id=...`.
- `POST /v1/workflow-runs/{id}`: `profile`, `session_id`, `expected_revision`, and `action` (`pause`, `cancel`, `resume`, `steer`). Resume includes `acknowledge_uncertain`; steer includes `text` and optional replacement `criteria`. Stale revisions conflict. Poll snapshots every two seconds while observing a run; deduplicate visible outputs by run ID plus event ID.

UUIDs identify runs and conversations; they are not authentication. Distributed workers, scheduling, nested/parallel workflows and CLI workflow commands are not included. Before rollback, pause/cancel runs and stop the host, retain run files, and back up the catalog. Restore the pre-workflow catalog if an older binary rejects its workflow fields; do not ask an older binary to resume new records.

## Checking configuration

`rynna doctor` validates the catalog without contacting a model provider, so a
configuration problem surfaces before a chat session or an unattended run hits
it.

```bash
rynna doctor
rynna doctor --config ./rynna.yaml --output json
```

For every profile it checks that:

- the catalog and the default profile resolve;
- each enabled provider has a provider to use, and any `api_key_env` variable it
  names is set;
- a `claude-subscription` program exists, or warns when a bare program name does
  not resolve on the current `PATH`;
- filesystem capability roots and command working directories exist and are
  directories;
- every mapped command program exists and is executable;
- every active skill has a readable `SKILL.md`.

It performs no network I/O and never reads or prints a credential value — it
reports only whether the named variable is set. All problems are reported in one
run rather than stopping at the first.

The exit status is `1` when any check fails and `0` otherwise, including when
only warnings are reported, so it can gate a deploy or wrap a cron entry:

```bash
rynna doctor && rynna run "summarize today's alerts"
```
