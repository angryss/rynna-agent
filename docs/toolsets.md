# Toolsets

Open **Settings → Toolsets**, select a profile, and choose **Configure** on a card. Clear or select **Enable**, then save. Web and desktop use the same profile settings and show the saved state; a failed save leaves the draft open and does not report success.

Toolsets group agent-invokable tools. In normal mode they are restrictive switches, not grants: enabling a group does not authorize writes or host programs, install skills, or create helpers. Project-scoped read/search tools are available by default without explicit capabilities. Other tools depend on actual configuration. A saved enabled switch alone must not be interpreted as an available executable tool.

| ID | Group | Tools |
| --- | --- | --- |
| `file_operations` | File Operations | `read_file`, `write_file`, `edit_file`, `search_files`, `find_files`, `list_directory`, `create_directory`, `file_info` |
| `code_search` | Code Search | `code_search` |
| `commands` | Commands | `run_command` |
| `skills` | Skills | `read_skill` |
| `subagents` | Subagents | `delegate_task` |

Rynna's exact-text patch tool is **`edit_file`**, not `patch`. The File Operations card uses the existing tool names. Indexed code search is a separate toolset; disable both File Operations and Code Search to remove both native file-reading paths. Command programs and MCP tools may independently access files: toolsets are not an OS sandbox.

## Persistence and APIs

Switches are stored inside each profile in the existing versioned catalog (`rynna.yaml`):

```yaml
profiles:
  local:
    # Existing provider and capability configuration stays here.
    disabled_toolsets: [commands, subagents]
```

Omitting `disabled_toolsets` (or using `[]`) permits all existing groups, preserving old configuration behavior. Unknown IDs are rejected. Settings follow profile renames and deletions because they are part of the profile itself. Use the normal profile read/update API (`GET /v1/profiles`, loopback-only `PUT /v1/profiles/{name}`) or Tauri profile commands; full-profile updates must preserve `disabled_toolsets` and `yolo` along with other settings. The UI does this when editing profiles, projects, models, and subagents.

CLI chat, CLI run, HTTP, desktop, and workflows share enforcement in `rynna-core`. CLI processes read saved settings at startup. Web/desktop saves change subsequent request snapshots immediately; requests already running keep their original immutable policy. Other running processes need a restart to load catalog changes made elsewhere, consistent with the catalog lifecycle.

## YOLO override

`Profile.yolo: true` or CLI `--yolo` bypasses these switches, native capability restrictions and execution budgets. Saved switches remain normal-mode preferences; show the effective YOLO override rather than claiming that a disabled group is blocked. No missing integration/helper/credential is created. OS inspection uses `run_command`, governed by existing command permissions and the Commands group in normal mode. See the [complete permission contract](../BACKEND-CONTRACT.md).

## Normal-mode enforcement

The core removes disabled tools from **both** the model-visible definitions and the executable lookup, after dynamic discovery and before constructing the delegation tool. A model requesting a disabled tool cannot execute it. Helpers receive only the parent's filtered tool snapshot. Code Search also gates an explicitly selected MCP `code_search` replacement. Other namespaced MCP tools remain controlled through MCP server settings.

Workflow steps use the same filters. A workflow cannot directly select a helper when the Subagents toolset is disabled. Toolset switches are included in the existing workflow policy fingerprint, so a saved policy change prevents continuing a workflow under its previous fingerprint; it does not retroactively cancel an in-flight step. Pause or cancel a run before changing its policy.

Toolsets narrow configured permissions. Filesystem root/deny/read-only rules, bounded command mappings, provider tool support, and workflow budgets continue to apply unchanged. Disabling Skills prevents `read_skill` invocation; it does not erase instructions already present in conversation history.
