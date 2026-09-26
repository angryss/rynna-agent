# Tool permissions and YOLO contract

## API / UI integration

`Profile.yolo` is a serialized boolean, defaulting to false for older profile files and clients. Profile create/update persists the submitted value. Same-name mode changes replace native adapters immediately for subsequent requests in both HTTP and desktop; any unfinished workflow must be completed or cancelled first. Runtime profiles report the effective mode; configured profiles report the persisted mode. A process started with CLI `--yolo` keeps that override even when a saved profile is changed back to false. Restart without the flag to remove the override.

`disabled_toolsets` is retained as a normal-mode preference and ignored in YOLO. Do not display a disabled switch as an effective restriction while YOLO is on. Native tool availability is provider-independent: API, subscription and fallback adapters receive the same Rynna tool definitions. Codex/Claude ambient tool isolation is unchanged; the shared Rynna executor remains the authority.

## Normal defaults

With no explicit filesystem capability, every composed profile receives:

- `read_file`, `file_info`, `list_directory`, `find_files`, `search_files`, `code_search`: read-only native access to the session's selected project, or the profile default directory / process cwd.

Relative paths use the project's starting directory. Absolute paths in another configured project directory are allowed; arbitrary outside paths, parent traversal, symlinks and default secret-deny patterns remain blocked. Selecting a different project creates a request-local binding, not a global directory change. Default writes and commands are **not advertised**. Explicit filesystem/command capabilities preserve their existing restrictions; toolset switches can hide them.

OS inspection uses `run_command` under the existing command permissions; no host-inspection tool or command capability is granted by default.

There is no general per-call permission-approval broker in this repository. Normal mode cannot promise an approval dialog or automatically grant write/terminal access. The existing setup path is a named filesystem capability with `read_only: false`, or a named command capability with its explicit executable map, selected in the profile. Capability changes still follow the existing configuration/restart behavior. This change does not claim to replace that setup with an interactive approval system.

## Explicit YOLO

Enable with global CLI `--yolo`, profile YAML `yolo: true`, or the profile API/UI setting. No Rynna confirmation prompt is emitted.

- Native read/write/edit/directory/search and `run_command` are supplied even with `capabilities: []`.
- Ignores filesystem root permissions, read-only mode, allow/deny/protected patterns, command executable maps and disabled toolsets.
- Paths may be absolute or relative to the selected starting directory. Explicit symlink targets are resolved with process authority. Native recursive search retains its no-follow traversal implementation; use an explicit linked path or `run_command` for alternative traversal.
- `run_command` accepts executable PATH names or paths plus a string argument array, inherits the process environment, uses the selected project cwd, and closes stdin. Shell syntax requires an explicit shell executable. Cancellation still terminates the process group.
- Ignores configured native filesystem read/search/traversal/output limits, command timeouts/output limits, core aggregate tool-call/result/deadline limits, model/tool turn budget, and workflow cumulative steps/tool calls/active-time budgets. Accounting remains recorded. Excessive work/output can exhaust process memory or provider quota; YOLO is not an isolation boundary.
- Configured disabled MCP servers are made discoverable on a request-local snapshot. Configured skills and helpers bypass their toolset switches. No missing server, skill, helper, executable, provider account or credential is invented.
- Helpers inherit the same mode and available tools. Workflows use the same runtime composition; a changed mode invalidates resumable execution context.

## Boundaries retained in YOLO

YOLO does **not** bypass kernel/file permissions, OS consent controls, provider authentication or missing credentials. It runs as the existing process identity, never automatically invokes sudo, changes credentials, or requests interactive authorization. A permission failure remains a tool error.

Argument/schema validation, UTF-8 text requirements, optional expected-hash/concurrent-write protection, regular-file checks, cancellation, model context capacity, workflow verification/state consistency, and integer/address-space limits remain. Commands retain a 128-argument / 32-KiB argument envelope. Helpers retain the 32-KiB task envelope and cannot recursively delegate. Workflow step result framing remains 32 KiB. Provider and MCP transport/schema/connection/message-size safety limits remain; these are not native-tool permission prompts. The indexed code-search protocol also retains its query/result framing and ignore-file behavior; `search_files` and commands provide alternatives.

This is intentionally not a claim of unlimited execution or automatic privilege elevation. Native arbitrary commands remain supported on Unix platforms only, as before; unsupported platforms report an error.

## Verification

Regression coverage includes real temporary-file reads/searches/writes, outside-project and symlink denials in normal mode, YOLO secret-pattern bypass, arbitrary shell execution with empty capabilities, configured timeout/output bypass, malformed argument rejection, subscription/API fallback composition, disabled MCP discovery, helper inheritance, workflow budget bypass, profile API persistence and immediate enable/disable, and the existing normal-mode filesystem/process security suite. Tests use isolated `XDG_CONFIG_HOME`; no real user configuration is modified. Authenticated live inference is a separate opt-in smoke test, not implied by fixture results.
