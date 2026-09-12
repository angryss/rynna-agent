mod code_search;

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use rynna_core::{Tool, ToolDefinition, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const DEFAULT_MAX_READ_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_TRAVERSAL_FILES: usize = 10_000;
const DEFAULT_MAX_TRAVERSAL_DEPTH: usize = 32;
const DEFAULT_MAX_SEARCH_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct FileSystemConfig {
    pub root: PathBuf,
    pub read_only: bool,
    pub allowed_patterns: Vec<String>,
    pub denied_patterns: Vec<String>,
    pub protected_patterns: Vec<String>,
    pub max_read_bytes: usize,
    pub max_results: usize,
    pub max_traversal_files: usize,
    pub max_traversal_depth: usize,
    pub max_search_bytes: usize,
    /// Override the private, persistent code index directory (primarily for embedding/tests).
    pub code_search_cache: Option<PathBuf>,
}

impl FileSystemConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            read_only: false,
            allowed_patterns: Vec::new(),
            denied_patterns: vec![
                ".env".to_owned(),
                ".env.*".to_owned(),
                "**/.env".to_owned(),
                "**/.env.*".to_owned(),
                "*.pem".to_owned(),
                "**/*.pem".to_owned(),
                "*.key".to_owned(),
                "**/*.key".to_owned(),
                "**/secrets*".to_owned(),
            ],
            protected_patterns: vec![".git/**".to_owned(), "**/.git/**".to_owned()],
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            max_results: 1000,
            max_traversal_files: DEFAULT_MAX_TRAVERSAL_FILES,
            max_traversal_depth: DEFAULT_MAX_TRAVERSAL_DEPTH,
            max_search_bytes: DEFAULT_MAX_SEARCH_BYTES,
            code_search_cache: None,
        }
    }
}

pub struct FileSystemToolset {
    inner: Arc<FileSystem>,
}

impl FileSystemToolset {
    pub fn new(config: FileSystemConfig) -> Result<Self, FileSystemError> {
        Ok(Self {
            inner: Arc::new(FileSystem::new(config)?),
        })
    }

    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        [
            Operation::ReadFile,
            Operation::WriteFile,
            Operation::EditFile,
            Operation::ListDirectory,
            Operation::FindFiles,
            Operation::SearchFiles,
            Operation::CreateDirectory,
            Operation::FileInfo,
        ]
        .into_iter()
        .map(|operation| {
            Arc::new(FileSystemTool {
                filesystem: Arc::clone(&self.inner),
                operation,
            }) as Arc<dyn Tool>
        })
        .chain(std::iter::once(
            Arc::new(code_search::CodeSearchTool::new(Arc::clone(&self.inner))) as Arc<dyn Tool>,
        ))
        .collect()
    }
}

struct FileSystem {
    config: FileSystemConfig,
    root: PathBuf,
    root_dir: Dir,
    allowed: GlobSet,
    denied: GlobSet,
    protected: GlobSet,
}

impl FileSystem {
    fn new(config: FileSystemConfig) -> Result<Self, FileSystemError> {
        let root = config
            .root
            .canonicalize()
            .map_err(|source| FileSystemError::Io {
                action: "canonicalize workspace root",
                path: config.root.clone(),
                source,
            })?;
        if !root.is_dir() {
            return Err(FileSystemError::InvalidRoot(root));
        }
        if config.max_read_bytes == 0
            || config.max_results == 0
            || config.max_traversal_files == 0
            || config.max_traversal_depth == 0
            || config.max_search_bytes == 0
        {
            return Err(FileSystemError::ZeroLimit);
        }
        let root_dir = Dir::open_ambient_dir(&root, ambient_authority()).map_err(|source| {
            FileSystemError::Io {
                action: "open workspace root",
                path: root.clone(),
                source,
            }
        })?;
        Ok(Self {
            allowed: compile_patterns(&config.allowed_patterns)?,
            denied: compile_patterns(&config.denied_patterns)?,
            protected: compile_patterns(&config.protected_patterns)?,
            config,
            root,
            root_dir,
        })
    }

    fn open_directory_nofollow(&self, path: &Path, write: bool) -> Result<Dir, FileSystemError> {
        let mut directory = self
            .root_dir
            .try_clone()
            .map_err(|source| FileSystemError::Io {
                action: "clone workspace root handle",
                path: self.root.clone(),
                source,
            })?;
        let mut cursor = PathBuf::new();
        for component in path.components() {
            if matches!(component, Component::CurDir) {
                continue;
            }
            cursor.push(component);
            let relative = relative_string(&cursor);
            self.ensure_traversal_allowed(&relative, write)?;
            directory = directory
                .open_dir_nofollow(component.as_os_str())
                .map_err(|source| {
                    self.nofollow_error(
                        &directory,
                        component.as_os_str(),
                        &relative,
                        "open directory component",
                        source,
                    )
                })?;
        }
        Ok(directory)
    }

    fn open_parent_nofollow(
        &self,
        path: &str,
        write: bool,
    ) -> Result<(Dir, PathBuf, String), FileSystemError> {
        let relative_path = validate_relative(path)?;
        let relative = relative_string(&relative_path);
        if write {
            self.ensure_write_allowed(&relative)?;
        } else {
            self.ensure_read_allowed(&relative)?;
        }
        let name = relative_path
            .file_name()
            .ok_or_else(|| FileSystemError::OutsideWorkspace(path.to_owned()))?;
        let parent_path = relative_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        Ok((
            self.open_directory_nofollow(parent_path, write)?,
            PathBuf::from(name),
            relative,
        ))
    }

    fn nofollow_error(
        &self,
        directory: &Dir,
        name: &std::ffi::OsStr,
        relative: &str,
        action: &'static str,
        source: std::io::Error,
    ) -> FileSystemError {
        if directory
            .symlink_metadata(name)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            FileSystemError::SymlinkPath(relative.to_owned())
        } else {
            FileSystemError::Io {
                action,
                path: self.root.join(relative),
                source,
            }
        }
    }

    fn ensure_read_allowed(&self, relative: &str) -> Result<(), FileSystemError> {
        if self.denied.is_match(relative)
            || (!self.config.allowed_patterns.is_empty() && !self.allowed.is_match(relative))
        {
            return Err(FileSystemError::Denied(relative.to_owned()));
        }
        Ok(())
    }

    fn ensure_traversal_allowed(&self, relative: &str, write: bool) -> Result<(), FileSystemError> {
        if self.denied.is_match(relative) {
            return Err(FileSystemError::Denied(relative.to_owned()));
        }
        if write && self.protected.is_match(relative) {
            return Err(FileSystemError::Protected(relative.to_owned()));
        }
        Ok(())
    }

    fn ensure_write_allowed(&self, relative: &str) -> Result<(), FileSystemError> {
        self.ensure_read_allowed(relative)?;
        if self.config.read_only {
            return Err(FileSystemError::ReadOnly);
        }
        if self.protected.is_match(relative) {
            return Err(FileSystemError::Protected(relative.to_owned()));
        }
        Ok(())
    }

    fn read_file(&self, path: &str) -> Result<Value, FileSystemError> {
        let (parent, name, relative) = self.open_parent_nofollow(path, false)?;
        let diagnostic_path = self.root.join(&relative);
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file = parent.open_with(&name, &options).map_err(|source| {
            self.nofollow_error(
                &parent,
                name.as_os_str(),
                &relative,
                "open file for reading",
                source,
            )
        })?;
        if !file
            .metadata()
            .map_err(|source| FileSystemError::Io {
                action: "inspect file",
                path: diagnostic_path.clone(),
                source,
            })?
            .is_file()
        {
            return Err(FileSystemError::NotRegularFile(relative));
        }
        let bytes = self.read_bounded_file(&mut file, &diagnostic_path, &relative)?;
        let content = String::from_utf8(bytes.clone())
            .map_err(|_| FileSystemError::BinaryFile(relative.clone()))?;
        Ok(json!({
            "path": relative,
            "content": content,
            "sha256": sha256(&bytes),
        }))
    }

    fn write_file(
        &self,
        path: &str,
        content: &str,
        expected_sha256: Option<&str>,
    ) -> Result<Value, FileSystemError> {
        let (parent, name, relative) = self.open_parent_nofollow(path, true)?;
        let diagnostic_path = self.root.join(&relative);
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(expected_sha256.is_none())
            .follow(FollowSymlinks::No)
            .nonblock(true);
        let mut file = parent.open_with(&name, &options).map_err(|source| {
            if expected_sha256.is_some() && source.kind() == std::io::ErrorKind::NotFound {
                FileSystemError::StaleWrite(relative.clone())
            } else {
                self.nofollow_error(
                    &parent,
                    name.as_os_str(),
                    &relative,
                    "open file for writing",
                    source,
                )
            }
        })?;
        if !file
            .metadata()
            .map_err(|source| FileSystemError::Io {
                action: "inspect file",
                path: diagnostic_path.clone(),
                source,
            })?
            .is_file()
        {
            return Err(FileSystemError::NotRegularFile(relative));
        }
        if let Some(expected) = expected_sha256 {
            let bytes = self.read_bounded_file(&mut file, &diagnostic_path, &relative)?;
            if sha256(&bytes) != expected {
                return Err(FileSystemError::StaleWrite(relative));
            }
        }
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.set_len(0))
            .and_then(|_| file.write_all(content.as_bytes()))
            .map_err(|source| FileSystemError::Io {
                action: "write file",
                path: diagnostic_path,
                source,
            })?;
        Ok(file_change_result(relative, content.as_bytes()))
    }

    fn edit_file(
        &self,
        path: &str,
        old_text: &str,
        new_text: &str,
        expected_sha256: Option<&str>,
    ) -> Result<Value, FileSystemError> {
        let (parent, name, relative) = self.open_parent_nofollow(path, true)?;
        let diagnostic_path = self.root.join(&relative);
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .follow(FollowSymlinks::No)
            .nonblock(true);
        let mut file = parent.open_with(&name, &options).map_err(|source| {
            self.nofollow_error(
                &parent,
                name.as_os_str(),
                &relative,
                "open file for editing",
                source,
            )
        })?;
        if !file
            .metadata()
            .map_err(|source| FileSystemError::Io {
                action: "inspect file",
                path: diagnostic_path.clone(),
                source,
            })?
            .is_file()
        {
            return Err(FileSystemError::NotRegularFile(relative));
        }
        let bytes = self.read_bounded_file(&mut file, &diagnostic_path, &relative)?;
        if expected_sha256.is_some_and(|expected| sha256(&bytes) != expected) {
            return Err(FileSystemError::StaleWrite(relative));
        }
        let content =
            String::from_utf8(bytes).map_err(|_| FileSystemError::BinaryFile(relative.clone()))?;
        if old_text.is_empty() || content.matches(old_text).count() != 1 {
            return Err(FileSystemError::EditMatch(relative));
        }
        let updated = content.replacen(old_text, new_text, 1);
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.set_len(0))
            .and_then(|_| file.write_all(updated.as_bytes()))
            .map_err(|source| FileSystemError::Io {
                action: "write edited file",
                path: diagnostic_path,
                source,
            })?;
        Ok(file_change_result(relative, updated.as_bytes()))
    }

    fn read_bounded_file(
        &self,
        file: &mut cap_std::fs::File,
        path: &Path,
        relative: &str,
    ) -> Result<Vec<u8>, FileSystemError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|source| FileSystemError::Io {
                action: "seek file",
                path: path.to_owned(),
                source,
            })?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(self.config.max_read_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| FileSystemError::Io {
                action: "read file",
                path: path.to_owned(),
                source,
            })?;
        if bytes.len() > self.config.max_read_bytes {
            return Err(FileSystemError::ReadLimit {
                path: relative.to_owned(),
                limit: self.config.max_read_bytes,
            });
        }
        Ok(bytes)
    }

    fn list_directory(&self, path: &str) -> Result<Value, FileSystemError> {
        let directory_path = validate_relative(path)?;
        let relative = relative_string(&directory_path);
        if !relative.is_empty() {
            self.ensure_traversal_allowed(&relative, false)?;
        }
        let diagnostic_path = self.root.join(&directory_path);
        let directory = self.open_directory_nofollow(&directory_path, false)?;
        let mut entries = Vec::new();
        let mut work_remaining = self.config.max_traversal_files;
        for entry in directory.entries().map_err(|source| FileSystemError::Io {
            action: "list directory",
            path: diagnostic_path.clone(),
            source,
        })? {
            if work_remaining == 0 {
                break;
            }
            work_remaining -= 1;
            let entry = entry.map_err(|source| FileSystemError::Io {
                action: "read directory entry",
                path: diagnostic_path.clone(),
                source,
            })?;
            let relative_path = directory_path.join(entry.file_name());
            let relative = relative_string(&relative_path);
            if self.ensure_read_allowed(&relative).is_err() {
                continue;
            }
            let metadata = match directory.symlink_metadata(entry.file_name()) {
                Ok(metadata)
                    if metadata.file_type().is_symlink()
                        || (!metadata.is_file() && !metadata.is_dir()) =>
                {
                    continue;
                }
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            entries.push(json!({
                "path": relative,
                "kind": file_kind(&metadata),
                "bytes": metadata.len(),
            }));
            if entries.len() >= self.config.max_results {
                break;
            }
        }
        entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
        Ok(json!({"entries": entries}))
    }

    fn find_files(&self, path: &str, pattern: &str) -> Result<Value, FileSystemError> {
        let matcher = compile_patterns(&[pattern.to_owned()])?;
        let paths = self.walk_files(path, |relative, _| (matcher.is_match(relative), false))?;
        Ok(json!({"paths": paths}))
    }

    fn search_files(
        &self,
        path: &str,
        pattern: &str,
        include_glob: Option<&str>,
    ) -> Result<Value, FileSystemError> {
        let regex = Regex::new(pattern)
            .map_err(|source| FileSystemError::InvalidRegex(pattern.to_owned(), source))?;
        let include = include_glob
            .map(|pattern| compile_patterns(&[pattern.to_owned()]))
            .transpose()?;
        let mut matches = Vec::new();
        let mut search_bytes = 0;
        self.walk_files(path, |relative, file| {
            if include
                .as_ref()
                .is_some_and(|matcher| !matcher.is_match(relative))
            {
                return (false, false);
            }
            let remaining = self.config.max_search_bytes - search_bytes;
            if remaining == 0 {
                return (false, true);
            }
            let read_limit = remaining.min(self.config.max_read_bytes);
            let Ok(bytes) = self.read_search_file(file, relative, read_limit) else {
                return (false, false);
            };
            search_bytes += bytes.len();
            let exhausted = search_bytes >= self.config.max_search_bytes;
            let Ok(content) = String::from_utf8(bytes) else {
                return (false, exhausted);
            };
            for (index, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    matches.push(json!({
                        "path": relative,
                        "line": index + 1,
                        "text": line,
                    }));
                    if matches.len() >= self.config.max_results {
                        return (false, true);
                    }
                }
            }
            (false, exhausted)
        })?;
        Ok(json!({"matches": matches}))
    }

    fn read_search_file(
        &self,
        file: &mut cap_std::fs::File,
        relative: &str,
        limit: usize,
    ) -> Result<Vec<u8>, FileSystemError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|source| FileSystemError::Io {
                action: "seek search file",
                path: self.root.join(relative),
                source,
            })?;
        let mut bytes = Vec::new();
        file.take(limit as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| FileSystemError::Io {
                action: "read search file",
                path: self.root.join(relative),
                source,
            })?;
        Ok(bytes)
    }

    fn walk_files(
        &self,
        path: &str,
        mut visit: impl FnMut(&str, &mut cap_std::fs::File) -> (bool, bool),
    ) -> Result<Vec<String>, FileSystemError> {
        let root_path = validate_relative(path)?;
        let root_relative = relative_string(&root_path);
        if !root_relative.is_empty() {
            self.ensure_traversal_allowed(&root_relative, false)?;
        }
        let directory = self.open_directory_nofollow(&root_path, false)?;
        let mut paths = Vec::new();
        let mut work_remaining = self.config.max_traversal_files;
        self.walk_directory(
            &directory,
            &root_path,
            0,
            &mut work_remaining,
            &mut paths,
            &mut visit,
        )?;
        paths.sort();
        Ok(paths)
    }

    fn walk_directory(
        &self,
        directory: &Dir,
        directory_path: &Path,
        depth: usize,
        work_remaining: &mut usize,
        paths: &mut Vec<String>,
        visit: &mut impl FnMut(&str, &mut cap_std::fs::File) -> (bool, bool),
    ) -> Result<bool, FileSystemError> {
        if *work_remaining == 0 {
            return Ok(true);
        }
        let iterator = directory.entries().map_err(|source| FileSystemError::Io {
            action: "read traversal directory",
            path: self.root.join(directory_path),
            source,
        })?;
        for entry in iterator {
            if *work_remaining == 0 {
                return Ok(true);
            }
            *work_remaining -= 1;
            let Ok(entry) = entry else {
                continue;
            };
            let relative_path = directory_path.join(entry.file_name());
            let relative = relative_string(&relative_path);
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_file() {
                if self.ensure_read_allowed(&relative).is_err() {
                    continue;
                }
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No).nonblock(true);
                let Ok(mut file) = directory.open_with(entry.file_name(), &options) else {
                    continue;
                };
                if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
                    continue;
                }
                let (matched, stop) = visit(&relative, &mut file);
                if matched {
                    paths.push(relative);
                }
                if stop || paths.len() >= self.config.max_results {
                    return Ok(true);
                }
            } else if kind.is_dir()
                && depth + 1 < self.config.max_traversal_depth
                && self.ensure_traversal_allowed(&relative, false).is_ok()
            {
                let Ok(child) = directory.open_dir_nofollow(entry.file_name()) else {
                    continue;
                };
                if self.walk_directory(
                    &child,
                    &relative_path,
                    depth + 1,
                    work_remaining,
                    paths,
                    visit,
                )? {
                    return Ok(true);
                }
            }
        }
        Ok(*work_remaining == 0)
    }

    fn create_directory(&self, path: &str) -> Result<Value, FileSystemError> {
        let relative_path = validate_relative(path)?;
        let relative = relative_string(&relative_path);
        self.ensure_write_allowed(&relative)?;
        let mut directory = self
            .root_dir
            .try_clone()
            .map_err(|source| FileSystemError::Io {
                action: "clone workspace root handle",
                path: self.root.clone(),
                source,
            })?;
        let mut cursor = PathBuf::new();
        for component in relative_path.components() {
            if matches!(component, Component::CurDir) {
                continue;
            }
            cursor.push(component);
            let component_relative = relative_string(&cursor);
            self.ensure_traversal_allowed(&component_relative, true)?;
            match directory.open_dir_nofollow(component.as_os_str()) {
                Ok(child) => directory = child,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    directory
                        .create_dir(component.as_os_str())
                        .map_err(|source| FileSystemError::Io {
                            action: "create directory component",
                            path: self.root.join(&component_relative),
                            source,
                        })?;
                    directory =
                        directory
                            .open_dir_nofollow(component.as_os_str())
                            .map_err(|source| {
                                self.nofollow_error(
                                    &directory,
                                    component.as_os_str(),
                                    &component_relative,
                                    "open created directory component",
                                    source,
                                )
                            })?;
                }
                Err(source) => {
                    return Err(self.nofollow_error(
                        &directory,
                        component.as_os_str(),
                        &component_relative,
                        "open directory component for creation",
                        source,
                    ));
                }
            }
        }
        Ok(json!({"path": relative}))
    }

    fn file_info(&self, path: &str) -> Result<Value, FileSystemError> {
        let relative_path = validate_relative(path)?;
        let relative = relative_string(&relative_path);
        self.ensure_read_allowed(&relative)?;
        let metadata = if relative.is_empty() {
            self.root_dir
                .metadata(".")
                .map_err(|source| FileSystemError::Io {
                    action: "inspect workspace root",
                    path: self.root.clone(),
                    source,
                })?
        } else {
            let (parent, name, relative) = self.open_parent_nofollow(path, false)?;
            let metadata = parent.symlink_metadata(&name).map_err(|source| {
                self.nofollow_error(&parent, name.as_os_str(), &relative, "inspect path", source)
            })?;
            if metadata.file_type().is_symlink() {
                return Err(FileSystemError::SymlinkPath(relative));
            }
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(FileSystemError::NotRegularFile(relative));
            }
            metadata
        };
        Ok(json!({
            "path": relative,
            "kind": file_kind(&metadata),
            "bytes": metadata.len(),
        }))
    }
}

#[derive(Clone, Copy)]
enum Operation {
    ReadFile,
    WriteFile,
    EditFile,
    ListDirectory,
    FindFiles,
    SearchFiles,
    CreateDirectory,
    FileInfo,
}

struct FileSystemTool {
    filesystem: Arc<FileSystem>,
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArguments {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArguments {
    path: String,
    content: String,
    expected_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArguments {
    path: String,
    old_text: String,
    new_text: String,
    expected_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArguments {
    path: String,
    pattern: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    path: String,
    pattern: String,
    include_glob: Option<String>,
}

#[async_trait]
impl Tool for FileSystemTool {
    fn workflow_policy(&self) -> String {
        format!("{:?}:{:?}", self.filesystem.root, self.filesystem.config)
    }
    fn definition(&self) -> ToolDefinition {
        match self.operation {
            Operation::ReadFile => path_tool_definition(
                "read_file",
                "Read a UTF-8 text file within the configured workspace",
            ),
            Operation::WriteFile => ToolDefinition::new(
                "write_file",
                "Create or replace a UTF-8 text file within the configured workspace",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"},
                        "expected_sha256": {"type": "string"}
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            ),
            Operation::EditFile => ToolDefinition::new(
                "edit_file",
                "Replace one exact text occurrence in a workspace file",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "old_text": {"type": "string"},
                        "new_text": {"type": "string"},
                        "expected_sha256": {"type": "string"}
                    },
                    "required": ["path", "old_text", "new_text"],
                    "additionalProperties": false
                }),
            ),
            Operation::ListDirectory => path_tool_definition(
                "list_directory",
                "List policy-visible entries in a workspace directory",
            ),
            Operation::FindFiles => ToolDefinition::new(
                "find_files",
                "Find workspace files whose relative paths match a glob",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "pattern": {"type": "string"}
                    },
                    "required": ["path", "pattern"],
                    "additionalProperties": false
                }),
            ),
            Operation::SearchFiles => ToolDefinition::new(
                "search_files",
                "Regex fallback for targeted searches; prefer code_search for indexed literal code searches",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "pattern": {"type": "string"},
                        "include_glob": {"type": "string"}
                    },
                    "required": ["path", "pattern"],
                    "additionalProperties": false
                }),
            ),
            Operation::CreateDirectory => path_tool_definition(
                "create_directory",
                "Create a directory and missing parents within the workspace",
            ),
            Operation::FileInfo => {
                path_tool_definition("file_info", "Inspect a workspace file or directory")
            }
        }
    }

    async fn execute(&self, arguments: Value) -> Result<Value, ToolError> {
        match self.operation {
            Operation::ReadFile => {
                let arguments: PathArguments = parse_arguments(arguments)?;
                self.filesystem.read_file(&arguments.path)
            }
            Operation::WriteFile => {
                let arguments: WriteArguments = parse_arguments(arguments)?;
                self.filesystem.write_file(
                    &arguments.path,
                    &arguments.content,
                    arguments.expected_sha256.as_deref(),
                )
            }
            Operation::EditFile => {
                let arguments: EditArguments = parse_arguments(arguments)?;
                self.filesystem.edit_file(
                    &arguments.path,
                    &arguments.old_text,
                    &arguments.new_text,
                    arguments.expected_sha256.as_deref(),
                )
            }
            Operation::ListDirectory => {
                let arguments: PathArguments = parse_arguments(arguments)?;
                self.filesystem.list_directory(&arguments.path)
            }
            Operation::FindFiles => {
                let arguments: FindArguments = parse_arguments(arguments)?;
                self.filesystem
                    .find_files(&arguments.path, &arguments.pattern)
            }
            Operation::SearchFiles => {
                let arguments: SearchArguments = parse_arguments(arguments)?;
                self.filesystem.search_files(
                    &arguments.path,
                    &arguments.pattern,
                    arguments.include_glob.as_deref(),
                )
            }
            Operation::CreateDirectory => {
                let arguments: PathArguments = parse_arguments(arguments)?;
                self.filesystem.create_directory(&arguments.path)
            }
            Operation::FileInfo => {
                let arguments: PathArguments = parse_arguments(arguments)?;
                self.filesystem.file_info(&arguments.path)
            }
        }
        .map_err(|error| ToolError::new(error.to_string()))
    }
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, ToolError> {
    serde_json::from_value(arguments)
        .map_err(|error| ToolError::new(format!("invalid arguments: {error}")))
}

fn path_tool_definition(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        description,
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn compile_patterns(patterns: &[String]) -> Result<GlobSet, FileSystemError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern).map_err(|source| FileSystemError::InvalidPattern {
                pattern: pattern.clone(),
                source,
            })?,
        );
    }
    builder.build().map_err(FileSystemError::BuildPatterns)
}

fn validate_relative(path: &str) -> Result<PathBuf, FileSystemError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(FileSystemError::OutsideWorkspace(
            path.display().to_string(),
        ));
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(FileSystemError::OutsideWorkspace(
                path.display().to_string(),
            ));
        }
    }
    Ok(path.to_owned())
}

fn relative_string(path: &Path) -> String {
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_change_result(path: String, bytes: &[u8]) -> Value {
    json!({"path": path, "sha256": sha256(bytes), "bytes": bytes.len()})
}

fn file_kind(metadata: &cap_std::fs::Metadata) -> &'static str {
    if metadata.is_file() {
        "file"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "other"
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileSystemError {
    #[error("filesystem path `{0}` is outside the workspace")]
    OutsideWorkspace(String),
    #[error("filesystem path `{0}` is denied by filesystem policy")]
    Denied(String),
    #[error("filesystem is read-only")]
    ReadOnly,
    #[error("filesystem path `{0}` is protected by filesystem policy")]
    Protected(String),
    #[error("filesystem refuses symlink path `{0}`")]
    SymlinkPath(String),
    #[error("filesystem file `{0}` changed since it was read")]
    StaleWrite(String),
    #[error("filesystem edit text must occur exactly once in `{0}`")]
    EditMatch(String),
    #[error("filesystem path `{0}` is not a regular file")]
    NotRegularFile(String),
    #[error("filesystem path `{0}` is not a directory")]
    NotDirectory(String),
    #[error("filesystem path `{0}` is a binary file")]
    BinaryFile(String),
    #[error("filesystem file `{path}` exceeds the {limit}-byte read limit")]
    ReadLimit { path: String, limit: usize },
    #[error("filesystem workspace root is not a directory: {}", .0.display())]
    InvalidRoot(PathBuf),
    #[error("filesystem limits must be greater than zero")]
    ZeroLimit,
    #[error("filesystem glob pattern `{pattern}` is invalid: {source}")]
    InvalidPattern {
        pattern: String,
        source: globset::Error,
    },
    #[error("failed to build filesystem glob policy: {0}")]
    BuildPatterns(globset::Error),
    #[error("filesystem search regular expression `{0}` is invalid: {1}")]
    InvalidRegex(String, regex::Error),
    #[error("failed to {action} at {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}
