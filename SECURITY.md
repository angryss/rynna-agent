# Security policy

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository. Do not disclose exploitable details in a public issue.

Include the affected version, reproduction steps, impact, and any suggested mitigation. Maintainers will acknowledge a complete report as soon as practical and coordinate disclosure after a fix is available.

## Deployment boundary

Rynna's HTTP service is not currently a public security boundary. It binds to loopback by default and should be exposed only through an authenticated TLS reverse proxy, VPN, or private network. Treat model-provider credentials and conversation contents as sensitive.

Profile catalogs may contain provider endpoints and MCP command definitions, so mount them read-only and do not expose them as static web assets. Store credentials only in the environment variables named by `api_key_env`; URL-embedded credentials are rejected. The `/v1/profiles` response intentionally omits provider URLs, credential-variable names, system prompts, and MCP command definitions.

Native command capabilities are disabled unless a profile explicitly activates one. Each configured alias resolves to one absolute executable path; Rynna invokes it directly without a shell, clears the inherited environment, supplies no stdin, and enforces argument-count, argument-byte, timeout, and combined-output limits. Configure the smallest possible executable set. Do not map a shell, interpreter, package manager, remote client, or other general-purpose executable unless the model is intentionally trusted with all authority that program and its arguments can exercise. A command can still read, modify, delete, or transmit anything available to Rynna's OS account. The application-level snapshot and retained-handle protections trust root and every other process running as Rynna's UID; same-UID processes can tamper with Rynna's private files and process state. Run command-enabled profiles as a dedicated low-privilege user, avoid sharing that UID, use read-only/narrow mounts where possible, and apply container or OS sandboxing for untrusted models or tenants.

Native filesystem capabilities must use the narrowest practical `root` and should use `read_only = true` unless writes are required. Rynna anchors operations to an open capability-directory handle and traverses tool-path components descriptor-relatively with no-follow semantics. Metadata operations do not open file content; content handles are opened nonblocking where supported and validated as regular files before I/O, so special files are rejected or skipped rather than consumed. All symlinks in tool paths are rejected, absolute and parent traversal is rejected, common secret files are denied by default, `.git` writes are protected by default, and total visited entries plus bytes actually read for search are bounded. Allow globs authorize final files and visible listing entries rather than required ancestor directories; deny policy still applies while traversing, and directory creation requires its final path to match the allowlist. These controls are not a hostile-tenant sandbox. VPS and container deployments should additionally expose only an intentionally scoped bind mount or volume, run as a dedicated unprivileged user, and use OS/container filesystem restrictions.

## MCP servers

MCP settings are profile-specific and stored in private `mcp.yaml` beside provider
settings. HTTP reads and writes require loopback access; the public profile list
does not expose commands or environment values. Local MCP commands run with the
Rynna process’s OS permissions and are not restricted by native tool allowlists.
Their environment is limited to ordinary path/home/temp resolution variables and
explicit `env` values. Remote tokens are read from `bearer_token_env`; HTTP redirects
are disabled. Tool results remain untrusted tool messages. Subscription providers
never discover or execute external MCP tools. Keep `mcp.yaml` out of source control
and static web directories; its editor intentionally displays configured values.
