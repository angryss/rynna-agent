# 0003. Native bounded command capability

- Status: accepted
- Date: 2026-08-26

## Context

Rynna needs to inspect and operate the computer where it runs, including answering questions such as which operating system is installed. A dedicated operating-system tool would solve only one prompt, while an implicit shell would grant broad ambient authority with weak policy and resource boundaries. The capability must work through the existing provider-neutral tool loop in CLI, server/web, and desktop modes without exposing executable paths through public profile metadata.

## Decision

Rynna provides an in-process `rynna-tools-command` adapter activated by a profile-scoped `kind = "command"` capability on Unix hosts. The adapter contributes one `run_command` tool. Each model-visible program alias maps to one operator-configured absolute executable path. When the profile is composed, Rynna opens authority-bearing paths nonblocking, validates the retained objects, and normally snapshots at most 64 MiB from the executable handle into a private execution directory. It also retains an open handle to the configured working directory and enters that exact directory descriptor in the child. Replacing either configured pathname later cannot substitute a different authorized object unless the actor can operate as root or Rynna's UID; those identities are trusted by this application-level boundary and can tamper with the private snapshot or process itself. The model supplies an alias and a string argument array; Rynna starts the prepared executable directly and never inserts an implicit shell. Executable updates take effect after Rynna restarts.

On macOS, mapped executables on the read-only root filesystem run from their canonical system paths instead of private copies: copied platform executables such as `uname` and `sw_vers` can be terminated by macOS. Rynna checks the opened file's read-only/root mount flags and verifies that the resolved path still identifies the same device and inode. Writable configured symlinks are resolved before execution, while executables outside that read-only root retain the private-copy behavior. The executable size limit and all command policies still apply.

Execution uses one retained working directory, null stdin, and an empty inherited environment. It accepts at most 128 arguments and 32 KiB of argument text. Catalog validation caps a configured call at 300 seconds and 1 MiB of combined stdout/stderr. Readers atomically reserve the shared quota before each read and consume at most one additional byte to detect overflow. Each command starts in a new process group; on timeout, output overflow, cancellation, or future drop, an independent supervisor task sends `SIGKILL` to that group and waits for Rynna's direct child. Orphaned descendants are reaped by the operating system, and cleanup failures are surfaced as tool errors and process diagnostics. Completed commands return success, optional exit code, stdout, and stderr, with output required to be valid UTF-8. The provider-neutral core also caps one response at 64 tool calls, five minutes of aggregate tool-loop time, and 8 MiB of serialized tool results. Catalog validation rejects empty program maps, relative executable paths, invalid limits, and multiple active command capabilities whose `run_command` names would conflict.

The built-in profile and example profiles do not activate command authority. Operators must deliberately add a command capability and should map only narrow programs. A macOS operator can map `/usr/bin/uname` and `/usr/bin/sw_vers` to support operating-system inspection. Mapping a shell, interpreter, package manager, network client, or other general-purpose executable intentionally grants the model that program's full authority.

## Consequences

- The same bounded command behavior is available through CLI, one-shot, HTTP/web, and Tauri desktop composition.
- Program paths and command policy remain private capability details; public profile metadata exposes only activation names.
- Direct execution avoids shell-string injection by default, while aliases keep platform paths out of model arguments.
- Clearing the environment prevents accidental provider-key and process-secret inheritance, but programs that require environment configuration need a future explicit environment policy rather than ambient inheritance.
- Unix is the current supported platform boundary; command-capability composition fails closed elsewhere. Linux and macOS command behavior is exercised in CI.
- Snapshotting executable bytes removes configured-path replacement from the execution boundary, but means operators must restart Rynna after updating a mapped executable.
- Root and every process sharing Rynna's UID are trusted; deployments should dedicate the UID to Rynna rather than sharing it with potentially hostile processes.
- Process groups bound descendants that remain in the group. A deliberately mapped program may still create a new session or otherwise use its full OS-account authority; only an OS sandbox can constrain that authority.
- Time and output limits are resource controls, not an authorization sandbox. Mapped programs can still exercise all permissions of Rynna's OS account.
- Untrusted deployments must use a dedicated low-privilege user, narrow/read-only mounts, containers, and OS sandboxing. Interactive approval policy remains future hardening work.
