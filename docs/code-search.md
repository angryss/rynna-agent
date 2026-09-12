# Indexed code search

The built-in `code_search` tool is installed automatically with every filesystem capability on tool-capable models. It searches case-sensitive literal substrings and returns workspace-relative paths, line numbers, and short snippets. For example:

```json
{"query":"UserService::create","path":"src","include_glob":"**/*.rs","max_results":20}
```

`path` defaults to the workspace root and selects a directory. `include_glob` matches workspace-relative paths. Queries require at least three Unicode characters, at most 1,024 UTF-8 bytes, and one line. `search_files` retains the existing bounded regex interface for regex searches and shorter literals. Tool descriptions direct the model to prefer indexed search; this is not semantic/vector search.

## Index lifecycle and large repositories

The first search streams each selected repository into its own persistent SQLite FTS5 trigram index. Session project directories select the repositories; overlapping directories and repeated repository roots are deduplicated. Git worktrees and nested repositories have separate indexes, with nested files excluded from their parent index. Directories without a Git marker use a directory-scoped fallback index. SQLite stores file content once in an external-content table and uses its inverted index to select candidate files. No model calls, embeddings, network service, or system SQLite installation are required. Bundled SQLite supplies FTS5 on supported platforms.

Every later search walks the selected repositories and compares file metadata with each repository’s stored fingerprints. Only added/changed files are read and hashed; content-identical changes update metadata without rebuilding FTS postings. Missing or newly excluded files are removed. Binary file fingerprints are remembered without storing their contents. On Unix, fingerprints include device, inode, size, modification time, and change time at nanosecond precision; other platforms use size, modification time, and creation time. Filesystems that do not reliably expose changes to those timestamps require clearing/rebuilding the cache.

Indexes survive process restarts and are reused when a repository appears in another session. Removing a repository from a session stops querying it without deleting or rebuilding its index. Policy changes select a separate index, so a permissive profile cannot populate a restrictive profile's search results. Index content is rechecked against live no-follow filesystem handles before snippets are returned. An error refreshing or querying a resolved repository produces `status: partial`, a failed-repository count, and an error summary; other selected repositories can still return results. Changes during a content read or traversal errors abort and roll back that repository’s refresh; retry the search. A race detected during result retrieval suppresses that file and sets `stale: true`.

Memory does not grow with the repository's full source contents: files are processed individually, SQLite's page-cache target is 8 MiB, and the temporary visited-path table is disk-backed. Warm refresh is still **O(number of visible directory entries)**, not O(number of edits); changed content I/O is proportional to changed bytes. There is no filesystem watcher or assumption that Git tracks every source file. Large repositories on network filesystems or slow disks will have slower metadata passes.

Indexing runs outside the async executor. A call waits up to 20 seconds and then returns `status: indexing` if work continues. This is a progress response, **not an empty search result**. Repeat the same query to retrieve the retained result when ready. Request-local project bindings share the underlying worker, so recreating a request snapshot does not restart a long search. The retained result is keyed by both query and session directories to prevent reuse across different sessions. Repository indexes are refreshed sequentially, with one SQLite connection open at a time to keep memory bounded. SQLite serializes writers across processes with a 30-second busy timeout; a competing process can report a busy error while another process performs a long build. The first worker continues. Background indexing continues if its waiting request is cancelled; process termination leaves the transaction to recover on reopening.

## Bounds and exclusions

- Nested `.gitignore` and `.ignore` files are evaluated before descent. `.git` directories, symlinks, special files, policy-denied paths, and files outside the allowlist are excluded. This does not load global Git excludes or `.git/info/exclude`.
- `max_read_bytes` applies per file (1 MiB by default). Larger files and binary/non-UTF-8 files are omitted; `index.files_skipped` reports their count. Use the existing filesystem capability setting to increase the per-file limit when needed.
- The existing `max_traversal_files`, `max_traversal_depth`, and `max_search_bytes` limits still apply to the legacy traversal/regex tools. They do not cap the repository index at 10,000 files or 16 MiB. Indexed traversal has a separate 128-directory-depth guard that returns an explicit error rather than silently dropping deeper source.
- Results default to 20 matches, with a caller maximum of 100 (also limited by the filesystem's `max_results`). Match objects and per-repository summaries share one 12,000-byte budget across the whole session; the small aggregate status/statistics envelope is additional. Snippets are cropped around the matching literal, with `snippet_truncated` reported per match.
- Queries examine at most 10,000 candidate files across all repository indexes combined. `truncated: true` indicates additional candidates or matches may exist; narrow the query, directory, or glob. This candidate limit does not limit indexed file count.
- A directory-scoped query uses the containing repository’s index and filters results to the requested/session directories. The full permitted repository is indexed for reuse. Repositories outside that intersection are not searched. Without an explicit session project, search defaults to the filesystem root and discovers nested Git repositories; loose files outside Git repositories use a fallback workspace index. Ignore generated/vendor trees to avoid unnecessary indexing.

Completed results include `status: ready` (or `partial` after a repository failure), `matches`, `truncated`, `stale`, `candidates_examined`, aggregate `index` counters, and per-repository `repositories` summaries. `repository_count` counts attempted indexes; `failed_repositories` counts failures. `summaries_truncated` reports omitted summaries when their shared byte budget is exhausted. Each match has a canonical `repository` root, a `repository_path` relative to that root, and the existing workspace-relative `path` suitable for `read_file`. `files_read`/`bytes_read` count source reads; ignore control files are read separately to detect rule changes. The index's disk use grows with source volume and trigram postings and is not capped automatically.

## Cache storage

Indexes live under the platform cache directory at `rynna/code-search-v1/<repository-and-policy-hash>/index.sqlite3` (on macOS, normally `~/Library/Caches/rynna/code-search-v1`). The private index directory has mode 0700 on Unix. Keys use the canonical repository root and the filesystem capability policy (including the workspace namespace in which its globs apply), never the session ID or repository list. Indexes contain copies of allowed source text. Secret exclusions come from the existing filesystem policy; index files are never committed into the searched repository. Embedders/tests may override the cache parent using `FileSystemConfig::code_search_cache`.

Stop Rynna and remove the relevant cache directory to reclaim disk space or force a rebuild. Old policy indexes are retained until removed. The next search recreates a missing index. Corrupt/unwritable databases produce an explicit per-repository error and partial search status; remove a damaged cache to rebuild it.

## Multi-repository sessions

Use a named project listing the repositories, for example this fragment within an existing profile:

```yaml
projects:
- name: application
  directories:
  - /work/api
  - /work/frontend
  default_directory: /work/api
```

The filesystem capability must already allow those directories, for example with root `/work` and appropriate allow/deny patterns. Project selection does not expand filesystem permissions. Invalid or inaccessible project directory selections reject the request before indexing begins. Absolute project directories must lie inside that root; relative project directories are interpreted within the filesystem root. Symlink directories and parent traversal are rejected.

A default `code_search` call in this project queries the separate API and frontend indexes. `path: api/src` narrows results to that directory and avoids scanning the frontend repository. Results retain workspace-relative paths such as `api/src/main.rs`, and also include `/work/api` as the repository root and `src/main.rs` as the repository-relative path. Repository roots are processed in stable path order, with discovered nested roots queued once; results are not globally relevance-ranked. A full result budget may stop retrieval from later repositories, while their indexes still refresh. `truncated` makes this explicit.

The existing CLI, HTTP, desktop, and delegated-agent project snapshots receive the bound tool through `Tool::for_project`. MCP replacement continues to replace the entire built-in search layer; a selected third-party provider owns its own multi-repository routing and schema, with the project directories available in the session context.

## Third-party replacement

Select the plugin's actual remote tool name on exactly one enabled MCP server in the profile's `mcp.yaml` settings:

```yaml
version: 1
profiles:
  default:
    mcpServers:
      search:
        enabled: true
        code_search_tool: search_code
        transport: stdio
        command: /absolute/path/to/search-plugin
        args: []
```

The same `code_search_tool` field works for Streamable HTTP servers and in the web/desktop MCP JSON editor. This example assumes your installed server advertises `search_code`; substitute its actual name and launch configuration.

The selected remote tool is advertised as `code_search`, replacing the built-in implementation and preserving the plugin's input schema, description, and protocol execution. Missing selected tools, blank selections, or multiple enabled search providers fail explicitly. Removing the selection or disabling that server restores the built-in default. Other MCP tools retain their namespaced names.

Native embedders can implement `ToolSource`, override `replaces_code_search()` to return true, and discover a tool named `code_search`. Ordinary tool sources cannot silently overwrite the built-in tool. Plugins own their repository access, indexing, result limits, and data handling, just as other configured MCP tools do; the native filesystem policy is not a sandbox around a third-party service. Subscription providers that disable external tools do not receive either search implementation.

## Reproducible scale test

Run the opt-in synthetic monorepo test:

```sh
cargo test -p rynna-tools-filesystem --test code_search large_repository -- --ignored --nocapture
RYNNA_SEARCH_BENCH_FILES=1000000 cargo test -p rynna-tools-filesystem --test code_search large_repository -- --ignored --nocapture
```

The default fixture has 100,000 files and 213,379,000 source bytes. It asserts that a file beyond the old traversal cutoff is found, reopening the index reads zero source files, changing one file reads/reindexes exactly that file, and the targeted query examines one candidate. The larger fixture accepts file counts that are positive multiples of 1,000.

Before repository partitioning was added, a local macOS debug-build run on 2026-09-12 measured 27.89 seconds cold, 1.43 seconds after reopening, and 1.37 seconds after one edit. The database occupied 646,729,728 bytes (about 3 times the source volume). These are synthetic measurements, not latency or compression guarantees for arbitrary repositories. Source repetition, disk speed, antivirus, concurrent work, file count, and file sizes affect results.

The one-million-file fixture also passed: 2,134,780,000 source bytes, 444.60 seconds cold, 26.70 seconds after reopening with zero source reads, and 27.61 seconds after one edit with exactly one source read. The database used 6,464,929,792 bytes. The targeted query examined one candidate and returned 284 bytes. The timed run reported peak resident memory of 105,267,200 bytes (100.4 MiB), including the test/Cargo process accounting; this is not a count of the operating system’s filesystem cache. Both warm operations exceeded the 20-second wait window and successfully retrieved their retained background results. This run overlapped other validation work; it demonstrates functional scale and bounded source reads, not an isolated performance comparison.

SQLite's [FTS5 trigram documentation](https://www.sqlite.org/fts5.html#the_trigram_tokenizer) describes the indexed substring mechanism and the three-character minimum.

The multi-repository regression can be run separately:

```sh
cargo test -p rynna-tools-filesystem --test multi_repository large_multi_repository -- --ignored --nocapture
```

It creates two independent repositories with 12,000 files each, asserts two separate databases and two matching candidates, reopens both without reading source, and changes exactly one file in one repository. A directory-scoped query also verifies that over 10,000 unrelated matches cannot exhaust its candidate budget. A local run measured 1.49 seconds cold and 0.303 seconds reopened; these small-file synthetic timings are not directly comparable to the larger padded-source fixtures.

After repository partitioning, the existing 100,000-file / 213 MB regression was rerun successfully: 39.95 seconds cold, 1.44 seconds reopened, and 1.48 seconds after one edit, with zero and one source reads respectively. It overlapped workspace compilation/testing. The indexed source size and database size were unchanged; result size increased to 712 bytes because repository identity and summary fields are now included. The one-million-file result above remains the pre-partitioning baseline, not a new run.
