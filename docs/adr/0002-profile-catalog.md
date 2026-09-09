# ADR 0002: Versioned profile catalog

## Status

Accepted

## Context

Rynna needs distinct local, work, automation, and hosted configurations without copying provider credentials or rebuilding each application surface. Profiles must select their own provider and model and must also scope capability activation such as skills and MCP servers. CLI, HTTP, browser, and desktop execution need the same profile semantics.

## Decision

Rynna uses a versioned YAML profile catalog owned by `rynna-config`.

- Providers are reusable named connection definitions with a provider kind, API base URL, and optional API-key environment-variable name.
- Profiles select one provider and model and carry a trusted system prompt, active skill names, and active MCP server names.
- MCP server definitions are named YAML mappings. Profiles reference them by name; invalid references fail configuration loading.
- Secrets are not stored in the catalog. Provider definitions name an environment variable whose value is read only by composition roots.
- The platform configuration path is `<config-dir>/rynna/config.yaml`. Rynna preserves its previous local Ollama behavior when that file and its legacy TOML predecessor are absent.
- `--config`/`RYNNA_CONFIG` selects an explicit catalog. `--profile`/`RYNNA_PROFILE` selects the process default.
- Existing provider/model/system-prompt CLI flags and environment variables override only the selected default profile.
- The core owns transport-independent profile metadata and dispatch. CLI, Axum, and Tauri compose concrete providers around it.
- The HTTP and Tauri adapters expose profile metadata and accept an optional profile on response requests. Browser and desktop clients clear conversation history when the user changes profiles.

Skill loading and MCP tool execution remain separate capabilities. This decision establishes their per-profile activation boundary without claiming that those future execution engines already exist.

## Consequences

- One server or desktop process can expose several configured profiles while keeping provider credentials out of API responses.
- Configuration errors are detected at startup rather than during an unrelated request.
- All providers exposed by a multi-profile process must currently be constructible at startup, including their referenced credential environment variables.
- Adding a new provider kind requires extending the config enum and each composition root's provider factory until a shared runtime composition crate becomes justified.
