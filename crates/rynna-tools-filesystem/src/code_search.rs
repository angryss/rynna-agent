//! Persistent, policy-scoped substring index. Directory enumeration and content reads
//! use the same no-follow capability handles as the other native filesystem tools.
use std::{
    collections::{BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, File, Metadata, OpenOptions};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rynna_core::{Tool, ToolDefinition, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{FileSystem, compile_patterns, relative_string, sha256, validate_relative};

type Result<T> = std::result::Result<T, ToolError>;
fn error(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(format!("code search: {error}"))
}

const MAX_OUTPUT_BYTES: usize = 12_000;
const MAX_CANDIDATES: usize = 10_000;
const MAX_DEPTH: usize = 128;

pub(super) struct CodeSearchTool {
    filesystem: Arc<FileSystem>,
    directories: Option<Vec<PathBuf>>,
    pending: Arc<tokio::sync::Mutex<Option<Pending>>>,
}

impl CodeSearchTool {
    pub fn new(filesystem: Arc<FileSystem>) -> Self {
        Self {
            filesystem,
            directories: None,
            pending: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}
fn indexing() -> Value {
    json!({"status":"indexing","matches":[],"truncated":true,
        "message":"The repository index is still being updated in the background. These are not search results. Retry code_search shortly."})
}

struct Pending {
    directories: Option<Vec<PathBuf>>,
    arguments: Arguments,
    task: tokio::task::JoinHandle<Result<Value>>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Arguments {
    query: String,
    #[serde(default = "root_path")]
    path: String,
    include_glob: Option<String>,
    #[serde(default = "default_results")]
    max_results: usize,
}
fn root_path() -> String {
    ".".into()
}
fn default_results() -> usize {
    20
}

#[async_trait]
impl Tool for CodeSearchTool {
    fn for_project(&self, directories: &[PathBuf]) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            filesystem: self.filesystem.clone(),
            directories: Some(directories.to_vec()),
            pending: self.pending.clone(),
        }))
    }
    fn workflow_policy(&self) -> String {
        format!(
            "code-search-v1:{:?}:{:?}:{:?}",
            self.filesystem.root, self.filesystem.config, self.directories
        )
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "code_search",
            "Preferred code search: case-sensitive literal substring search using a persistent incremental repository index. Query must contain at least 3 characters on one line. Returns bounded path/line snippets. Searches the session repositories using a separate persistent index for each; later searches update changes. Results identify repository roots and workspace-relative paths. Honors filesystem policy and .gitignore/.ignore. Use search_files for short literals or regex in a narrow directory.",
            json!({"type":"object", "properties": {
                "query":{"type":"string","minLength":3,"maxLength":1024},
                "path":{"type":"string","description":"Workspace-relative directory; defaults to ."},
                "include_glob":{"type":"string"},
                "max_results":{"type":"integer","minimum":1,"maximum":100}
            }, "required":["query"],"additionalProperties":false}),
        )
    }
    async fn execute(&self, arguments: Value) -> Result<Value> {
        let arguments: Arguments = serde_json::from_value(arguments).map_err(error)?;
        if arguments.query.chars().count() < 3
            || arguments.query.len() > 1024
            || arguments.query.contains(['\n', '\r', '\0'])
            || !(1..=100).contains(&arguments.max_results)
        {
            return Err(error(
                "use a single-line literal of 3–1024 bytes and max_results between 1 and 100",
            ));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut pending = match tokio::time::timeout_at(deadline, self.pending.lock()).await {
            Ok(pending) => pending,
            Err(_) => return Ok(indexing()),
        };
        if let Some(work) = pending.as_mut() {
            let same_query = work.arguments == arguments && work.directories == self.directories;
            match tokio::time::timeout_at(deadline, &mut work.task).await {
                Ok(result) => {
                    *pending = None;
                    let result = result.map_err(error)??;
                    if same_query {
                        return Ok(result);
                    }
                }
                Err(_) => return Ok(indexing()),
            }
        }
        let filesystem = self.filesystem.clone();
        let work_arguments = arguments.clone();
        let directories = self.directories.clone();
        // Retain the handle across calls: even a warm scan taking over twenty
        // seconds must eventually deliver its result instead of restarting.
        *pending = Some(Pending {
            directories: self.directories.clone(),
            arguments,
            task: tokio::task::spawn_blocking(move || {
                search(&filesystem, work_arguments, directories.as_deref())
            }),
        });
        let task = &mut pending.as_mut().expect("pending search").task;
        match tokio::time::timeout_at(deadline, task).await {
            Ok(result) => {
                *pending = None;
                result.map_err(error)?
            }
            Err(_) => Ok(indexing()),
        }
    }
}

#[derive(Default, Serialize)]
struct Stats {
    files_checked: usize,
    files_read: usize,
    files_updated: usize,
    files_removed: usize,
    files_skipped: usize,
    bytes_read: u64,
}

fn open_file(directory: &Dir, name: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = directory.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(file)
}

fn fingerprint(metadata: &Metadata) -> String {
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )
    }
    #[cfg(not(unix))]
    {
        format!(
            "{}:{:?}:{:?}",
            metadata.len(),
            metadata.modified(),
            metadata.created()
        )
    }
}

fn repository_identity(filesystem: &FileSystem, repository: &Path) -> PathBuf {
    if repository.as_os_str().is_empty() {
        filesystem.root.clone()
    } else {
        filesystem.root.join(repository)
    }
}

fn database(
    filesystem: &FileSystem,
    repository: &Path,
) -> Result<(Connection, std::path::PathBuf)> {
    let cache = filesystem
        .config
        .code_search_cache
        .clone()
        .or_else(|| dirs::cache_dir().map(|path| path.join("rynna/code-search-v1")))
        .ok_or_else(|| error("no cache directory available"))?;
    let key = sha256(
        format!(
            "v1:{:?}:{:?}",
            repository_identity(filesystem, repository),
            filesystem.config
        )
        .as_bytes(),
    );
    let cache = cache.join(key);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&cache).map_err(error)?;
    if std::fs::symlink_metadata(&cache)
        .map_err(error)?
        .file_type()
        .is_symlink()
    {
        return Err(error("index directory must not be a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).map_err(error)?;
    }
    let database = cache.join("index.sqlite3");
    if std::fs::symlink_metadata(&database).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(error("index database must not be a symlink"));
    }
    let connection = Connection::open(database).map_err(error)?;
    connection
        .busy_timeout(Duration::from_secs(30))
        .map_err(error)?;
    connection
        .execute_batch(
            "PRAGMA cache_size = -8192;
        PRAGMA temp_store = FILE;
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
            fingerprint TEXT NOT NULL, digest TEXT NOT NULL, content TEXT NOT NULL);
        CREATE VIRTUAL TABLE IF NOT EXISTS code USING fts5(
            content, content='files', content_rowid='id', tokenize='trigram case_sensitive 1');
        CREATE TRIGGER IF NOT EXISTS files_insert AFTER INSERT ON files BEGIN
            INSERT INTO code(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS files_delete AFTER DELETE ON files BEGIN
            INSERT INTO code(code, rowid, content) VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS files_update AFTER UPDATE OF content ON files BEGIN
            INSERT INTO code(code, rowid, content) VALUES ('delete', old.id, old.content);
            INSERT INTO code(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TEMP TABLE seen (path TEXT PRIMARY KEY) WITHOUT ROWID;",
        )
        .map_err(error)?;
    Ok((connection, cache.canonicalize().map_err(error)?))
}

fn ignore_rules(filesystem: &FileSystem, directory: &Dir, path: &Path) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(path);
    for name in [".gitignore", ".ignore"] {
        let relative = relative_string(&path.join(name));
        if filesystem.ensure_read_allowed(&relative).is_err() {
            continue;
        }
        match open_file(directory, Path::new(name)) {
            Ok(mut file) => {
                let bytes = filesystem
                    .read_search_file(
                        &mut file,
                        &relative,
                        filesystem.config.max_read_bytes.saturating_add(1),
                    )
                    .map_err(error)?;
                if bytes.len() > filesystem.config.max_read_bytes {
                    return Err(error("ignore file exceeds per-file read limit"));
                }
                let content = std::str::from_utf8(&bytes).map_err(error)?;
                for line in content.lines() {
                    builder.add_line(None, line).map_err(error)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_)
                if directory
                    .symlink_metadata(name)
                    .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) => {}
            Err(e) => return Err(error(e)),
        }
    }
    builder.build().map_err(error)
}

struct Indexer<'a> {
    filesystem: &'a FileSystem,
    transaction: &'a Transaction<'a>,
    stats: Stats,
    cache_directory: &'a Path,
    nested_repositories: Vec<PathBuf>,
}
impl Indexer<'_> {
    fn file(&mut self, directory: &Dir, name: &Path, relative: &str) -> Result<()> {
        self.stats.files_checked += 1;
        // Opening only changed files avoids the cost of a descriptor per source on warm scans.
        let metadata = directory.symlink_metadata(name).map_err(error)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.len() > self.filesystem.config.max_read_bytes as u64 {
            self.stats.files_skipped += 1;
            return Ok(());
        }
        let stamp = fingerprint(&metadata);
        self.transaction
            .prepare_cached("INSERT INTO seen VALUES (?1)")
            .map_err(error)?
            .execute([relative])
            .map_err(error)?;
        let previous: Option<(String, String)> = self
            .transaction
            .prepare_cached("SELECT fingerprint, digest FROM files WHERE path=?1")
            .map_err(error)?
            .query_row([relative], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(error)?;
        if previous.as_ref().is_some_and(|(old, _)| *old == stamp) {
            if previous
                .as_ref()
                .is_some_and(|(_, digest)| digest.starts_with("binary:"))
            {
                self.stats.files_skipped += 1;
            }
            return Ok(());
        }
        let mut file = open_file(directory, name).map_err(error)?;
        let before = fingerprint(&file.metadata().map_err(error)?);
        if before != stamp {
            return Err(error("file changed during indexing; retry search"));
        }
        let bytes = self
            .filesystem
            .read_search_file(
                &mut file,
                relative,
                self.filesystem.config.max_read_bytes.saturating_add(1),
            )
            .map_err(error)?;
        self.stats.files_read += 1;
        self.stats.bytes_read += bytes.len() as u64;
        if fingerprint(&file.metadata().map_err(error)?) != before {
            return Err(error("file changed during indexing; retry search"));
        }
        let content = std::str::from_utf8(&bytes)
            .ok()
            .filter(|content| !content.contains('\0'));
        let skipped = content.is_none();
        if skipped {
            self.stats.files_skipped += 1;
        }
        let digest = if skipped {
            format!("binary:{}", sha256(&bytes))
        } else {
            sha256(&bytes)
        };
        if previous.as_ref().is_some_and(|(_, old)| *old == digest) {
            self.transaction
                .prepare_cached("UPDATE files SET fingerprint=?1 WHERE path=?2")
                .map_err(error)?
                .execute(params![stamp, relative])
                .map_err(error)?;
            return Ok(());
        }
        // Cache binary fingerprints too, without storing their bytes or rereading
        // them on every warm scan. Their empty FTS document cannot produce hits.
        let content = content.unwrap_or("");
        self.transaction.prepare_cached("INSERT INTO files(path,fingerprint,digest,content) VALUES (?1,?2,?3,?4)
            ON CONFLICT(path) DO UPDATE SET fingerprint=excluded.fingerprint,digest=excluded.digest,content=excluded.content")
            .map_err(error)?.execute(params![relative, stamp, digest, content]).map_err(error)?;
        if !skipped {
            self.stats.files_updated += 1;
        }
        Ok(())
    }

    fn directory(
        &mut self,
        directory: &Dir,
        path: &Path,
        ignores: &mut Vec<Gitignore>,
    ) -> Result<()> {
        if ignores.len() >= MAX_DEPTH {
            return Err(error(
                "repository exceeds indexing depth limit (128); use a narrower filesystem root",
            ));
        }
        ignores.push(ignore_rules(self.filesystem, directory, path)?);
        for entry in directory.entries().map_err(error)? {
            let entry = entry.map_err(error)?;
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            let child_path = path.join(&name);
            if self.filesystem.root.join(&child_path) == self.cache_directory {
                continue;
            }
            let relative = relative_string(&child_path);
            if self
                .filesystem
                .ensure_traversal_allowed(&relative, false)
                .is_err()
            {
                continue;
            }
            let kind = entry.file_type().map_err(error)?;
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                continue;
            }
            let ignored = ignores
                .iter()
                .rev()
                .find_map(|ignore| {
                    let matched = ignore.matched(&child_path, kind.is_dir());
                    if matched.is_none() {
                        None
                    } else {
                        Some(matched.is_ignore())
                    }
                })
                .unwrap_or(false);
            if ignored {
                continue;
            }
            if kind.is_dir() {
                let child = directory.open_dir_nofollow(&name).map_err(error)?;
                if is_repository(&child) {
                    self.nested_repositories.push(child_path);
                } else {
                    self.directory(&child, &child_path, ignores)?;
                }
            } else if self.filesystem.ensure_read_allowed(&relative).is_ok() {
                self.file(directory, Path::new(&name), &relative)?;
            }
        }
        ignores.pop();
        Ok(())
    }
}

// Project paths select scope; they never grant access outside the filesystem capability.
fn relative_directory(filesystem: &FileSystem, directory: &Path) -> Result<PathBuf> {
    let relative = if directory.is_absolute() {
        directory
            .strip_prefix(&filesystem.root)
            .or_else(|_| {
                if filesystem.config.root.is_absolute() {
                    directory.strip_prefix(&filesystem.config.root)
                } else {
                    directory.strip_prefix(&filesystem.root)
                }
            })
            .map_err(|_| error("session repository is outside the filesystem capability"))?
    } else {
        directory
    };
    let text = relative_string(relative);
    // Validate before normalization so parent traversal is never erased.
    if !relative.as_os_str().is_empty() {
        validate_relative(&relative.to_string_lossy()).map_err(error)?;
    }
    let relative = PathBuf::from(text);
    filesystem
        .open_directory_nofollow(&relative, false)
        .map_err(error)?;
    Ok(relative)
}

fn is_repository(directory: &Dir) -> bool {
    directory.symlink_metadata(".git").is_ok_and(|metadata| {
        !metadata.file_type().is_symlink() && (metadata.is_dir() || metadata.is_file())
    })
}

fn repository_root(filesystem: &FileSystem, scope: &Path, fallback: &Path) -> Result<PathBuf> {
    for ancestor in scope.ancestors() {
        let directory = filesystem
            .open_directory_nofollow(ancestor, false)
            .map_err(error)?;
        if is_repository(&directory) {
            return Ok(ancestor.to_owned());
        }
    }
    Ok(fallback.to_owned())
}

fn ancestor_ignores(filesystem: &FileSystem, repository: &Path) -> Result<Vec<Gitignore>> {
    let mut ancestors: Vec<_> = repository.ancestors().skip(1).collect();
    ancestors.reverse();
    let mut ignores: Vec<Gitignore> = Vec::new();
    for ancestor in ancestors.into_iter().chain(std::iter::once(repository)) {
        if ignores
            .iter()
            .rev()
            .find_map(|rules| {
                let matched = rules.matched(ancestor, true);
                if matched.is_none() {
                    None
                } else {
                    Some(matched.is_ignore())
                }
            })
            .unwrap_or(false)
        {
            return Err(error("repository is excluded by workspace ignore rules"));
        }
        if ancestor != repository {
            let directory = filesystem
                .open_directory_nofollow(ancestor, false)
                .map_err(error)?;
            ignores.push(ignore_rules(filesystem, &directory, ancestor)?);
        }
    }
    Ok(ignores)
}

#[derive(Default)]
struct SearchResults {
    matches: Vec<Value>,
    repositories: Vec<Value>,
    stats: Stats,
    bytes: usize,
    candidates: usize,
    repository_count: usize,
    failed_repositories: usize,
    summaries_truncated: bool,
    truncated: bool,
    stale: bool,
}
impl SearchResults {
    fn add_stats(&mut self, stats: &Stats) {
        self.stats.files_checked += stats.files_checked;
        self.stats.files_read += stats.files_read;
        self.stats.files_updated += stats.files_updated;
        self.stats.files_removed += stats.files_removed;
        self.stats.files_skipped += stats.files_skipped;
        self.stats.bytes_read += stats.bytes_read;
    }
    fn summary(&mut self, summary: Value) {
        let size = summary.to_string().len();
        if self.bytes + size <= MAX_OUTPUT_BYTES {
            self.bytes += size;
            self.repositories.push(summary);
        } else {
            self.summaries_truncated = true;
        }
    }
    fn finish(self) -> Value {
        json!({"status":if self.failed_repositories == 0 { "ready" } else { "partial" },
            "matches":self.matches,"truncated":self.truncated,"stale":self.stale,
            "index":self.stats,"candidates_examined":self.candidates,
            "repositories":self.repositories,"repository_count":self.repository_count,
            "failed_repositories":self.failed_repositories,"summaries_truncated":self.summaries_truncated})
    }
}

fn search(
    filesystem: &FileSystem,
    arguments: Arguments,
    directories: Option<&[PathBuf]>,
) -> Result<Value> {
    let query_path = validate_relative(&arguments.path).map_err(error)?;
    let query_path = PathBuf::from(relative_string(&query_path));
    filesystem
        .open_directory_nofollow(&query_path, false)
        .map_err(error)?;
    if let Some(glob) = &arguments.include_glob {
        compile_patterns(std::slice::from_ref(glob)).map_err(error)?;
    }
    let default = [PathBuf::from(".")];
    let selected = directories
        .unwrap_or(&default)
        .iter()
        .map(|directory| relative_directory(filesystem, directory))
        .collect::<Result<Vec<_>>>()?;
    let mut scopes = BTreeSet::new();
    let mut repositories = BTreeSet::new();
    for directory in selected {
        let scope = if directory.starts_with(&query_path) {
            directory.clone()
        } else if query_path.starts_with(&directory) {
            query_path.clone()
        } else {
            continue;
        };
        repositories.insert(repository_root(filesystem, &scope, &directory)?);
        scopes.insert(scope);
    }
    if scopes.is_empty() {
        return Err(error("search path is outside the session repositories"));
    }
    // Overlapping non-Git project directories belong to their ancestor's fallback index.
    let mut queue: VecDeque<_> = repositories
        .iter()
        .filter(|repository| {
            filesystem
                .open_directory_nofollow(repository, false)
                .is_ok_and(|directory| is_repository(&directory))
                || !repositories
                    .iter()
                    .any(|parent| parent != *repository && repository.starts_with(parent))
        })
        .cloned()
        .collect();
    let scopes: Vec<_> = scopes.into_iter().collect();
    let mut visited = BTreeSet::new();
    let mut output = SearchResults::default();
    while let Some(repository) = queue.pop_front() {
        if !scopes
            .iter()
            .any(|scope| scope.starts_with(&repository) || repository.starts_with(scope))
            || !visited.insert(repository.clone())
        {
            continue;
        }
        output.repository_count += 1;
        match search_repository(filesystem, &repository, &scopes, &arguments, &mut output) {
            Ok(nested) => queue.extend(nested),
            Err(reason) => {
                output.failed_repositories += 1;
                output.truncated = true;
                output.summary(
                    json!({"repository":repository_identity(filesystem, &repository),
                    "error":reason.to_string().chars().take(512).collect::<String>()}),
                );
            }
        }
    }
    Ok(output.finish())
}

fn search_repository(
    filesystem: &FileSystem,
    repository: &Path,
    scopes: &[PathBuf],
    arguments: &Arguments,
    output: &mut SearchResults,
) -> Result<Vec<PathBuf>> {
    let include = arguments
        .include_glob
        .as_ref()
        .map(|glob| compile_patterns(std::slice::from_ref(glob)))
        .transpose()
        .map_err(error)?;
    let mut ignores = ancestor_ignores(filesystem, repository)?;
    let (mut connection, cache_directory) = database(filesystem, repository)?;
    // One transaction gives concurrent processes a consistent index and rolls back
    // failed traversals rather than pruning unseen files after an incomplete scan.
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let mut indexer = Indexer {
        filesystem,
        transaction: &transaction,
        stats: Stats::default(),
        cache_directory: &cache_directory,
        nested_repositories: Vec::new(),
    };
    let directory = filesystem
        .open_directory_nofollow(repository, false)
        .map_err(error)?;
    indexer.directory(&directory, repository, &mut ignores)?;
    indexer.stats.files_removed = transaction
        .execute(
            "DELETE FROM files WHERE path NOT IN (SELECT path FROM seen)",
            [],
        )
        .map_err(error)?;
    let stats = indexer.stats;
    let nested = indexer.nested_repositories;
    transaction.commit().map_err(error)?;
    output.add_stats(&stats);
    output.summary(json!({"repository":repository_identity(filesystem, repository),"index":stats}));
    if output.matches.len() >= arguments.max_results.min(filesystem.config.max_results)
        || output.candidates >= MAX_CANDIDATES
    {
        output.truncated = true;
        return Ok(nested);
    }

    connection
        .execute_batch("CREATE TEMP TABLE search_scopes(prefix TEXT NOT NULL)")
        .map_err(error)?;
    for scope in scopes {
        let relative = relative_string(scope);
        let prefix = if relative.is_empty() {
            relative
        } else {
            format!("{relative}/")
        };
        connection
            .execute("INSERT INTO search_scopes VALUES (?1)", [prefix])
            .map_err(error)?;
    }
    let phrase = format!("\"{}\"", arguments.query.replace('"', "\"\""));
    let mut statement = connection
        .prepare(
            "SELECT files.path, files.content, files.fingerprint
        FROM code JOIN files ON files.id=code.rowid
        WHERE code MATCH ?1 AND EXISTS (SELECT 1 FROM search_scopes WHERE substr(files.path,1,length(prefix))=prefix)",
        )
        .map_err(error)?;
    let mut rows = statement.query([phrase]).map_err(error)?;
    let mut stop = false;
    while let Some(row) = rows.next().map_err(error)? {
        if output.candidates >= MAX_CANDIDATES {
            output.truncated = true;
            break;
        }
        output.candidates += 1;
        let relative: String = row.get(0).map_err(error)?;
        if include
            .as_ref()
            .is_some_and(|glob| !glob.is_match(&relative))
        {
            continue;
        }
        // Recheck the live capability before returning persisted source text.
        let live = filesystem
            .open_parent_nofollow(&relative, false)
            .ok()
            .and_then(|(parent, name, _)| open_file(&parent, &name).ok())
            .and_then(|file| file.metadata().ok());
        let stamp: String = row.get(2).map_err(error)?;
        if live.is_none_or(|metadata| fingerprint(&metadata) != stamp) {
            output.stale = true;
            continue;
        }
        let content: String = row.get(1).map_err(error)?;
        for (line_number, line) in content.lines().enumerate() {
            let Some(position) = line.find(&arguments.query) else {
                continue;
            };
            let mut start = position.saturating_sub(120);
            while !line.is_char_boundary(start) {
                start += 1;
            }
            let mut end = (position + arguments.query.len() + 200).min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            let item = json!({"repository":repository_identity(filesystem, repository), "repository_path":relative_string(Path::new(&relative).strip_prefix(repository).map_err(error)?), "path":relative,"line":line_number+1,"text":&line[start..end],
                "snippet_truncated":start != 0 || end != line.len()});
            let size = serde_json::to_vec(&item).map_err(error)?.len();
            if output.matches.len() >= arguments.max_results.min(filesystem.config.max_results)
                || output.bytes + size > MAX_OUTPUT_BYTES
            {
                output.truncated = true;
                stop = true;
                break;
            }
            output.bytes += size;
            output.matches.push(item);
        }
        if stop {
            break;
        }
    }
    Ok(nested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn long_running_search_keeps_its_result_for_the_next_call() {
        let root = tempfile::tempdir().unwrap();
        let filesystem =
            Arc::new(FileSystem::new(super::super::FileSystemConfig::new(root.path())).unwrap());
        let tool = CodeSearchTool::new(filesystem);
        *tool.pending.lock().await = Some(Pending {
            directories: Some(vec![PathBuf::from(".")]),
            arguments: serde_json::from_value(json!({"query":"needle"})).unwrap(),
            task: tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(25)).await;
                Ok(json!({"status":"ready","matches":["completed"]}))
            }),
        });
        let bound = tool.for_project(&[PathBuf::from(".")]).unwrap();
        let first = bound.execute(json!({"query":"needle"})).await.unwrap();
        assert_eq!(first["status"], "indexing");
        let next_request = tool.for_project(&[PathBuf::from(".")]).unwrap();
        let second = next_request
            .execute(json!({"query":"needle"}))
            .await
            .unwrap();
        assert_eq!(second["matches"], json!(["completed"]));
        assert!(tool.pending.lock().await.is_none());
    }
    #[tokio::test]
    async fn completed_background_result_is_not_reused_for_a_different_session() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("b")).unwrap();
        std::fs::write(root.path().join("b/main.rs"), "needle in b").unwrap();
        let mut config = super::super::FileSystemConfig::new(root.path());
        config.code_search_cache = Some(cache.path().to_owned());
        let tool = CodeSearchTool::new(Arc::new(FileSystem::new(config).unwrap()));
        *tool.pending.lock().await = Some(Pending {
            directories: Some(vec![PathBuf::from("a")]),
            arguments: serde_json::from_value(json!({"query":"needle"})).unwrap(),
            task: tokio::spawn(async { Ok(json!({"matches":[{"path":"a/old-result"}]})) }),
        });
        let other = tool.for_project(&[PathBuf::from("b")]).unwrap();
        let result = other.execute(json!({"query":"needle"})).await.unwrap();
        assert_eq!(result["matches"][0]["path"], "b/main.rs");
    }
}
