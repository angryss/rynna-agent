use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use rynna_core::{Profile, ProfileProvider, Workspace};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

pub mod mcp;
pub mod memory;
pub mod profile_update;

const MAX_COMMAND_TIMEOUT_SECONDS: u64 = 300;
const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;

pub const DEFAULT_API_BASE: &str = "http://127.0.0.1:11434/v1";
pub const DEFAULT_MODEL: &str = "qwen3:8b";
pub const DEFAULT_PROFILE: &str = "default";
pub const DEFAULT_PROVIDER: &str = "ollama";
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are Rynna, a careful and capable AI software agent.";
pub const OPENAI_ACCOUNT_PROFILE: &str = "openai-account";
const CONFIG_VERSION: u32 = 1;
const PROVIDER_SETTINGS_VERSION: u32 = 1;
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiAuthentication {
    ApiKey,
    Chatgpt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicAuthentication {
    ApiKey,
    Subscription,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfiguredProvider {
    Ollama {
        api_base: String,
    },
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "openai")]
    OpenAi {
        authentication: OpenAiAuthentication,
        #[serde(default)]
        reuse_existing: bool,
    },
    Anthropic {
        authentication: AnthropicAuthentication,
    },
}

impl ConfiguredProvider {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Ollama { .. } => "ollama",
            Self::OpenRouter => "openrouter",
            Self::OpenAi { .. } => "openai",
            Self::Anthropic { .. } => "anthropic",
        }
    }

    fn validate(&self) -> Result<(), ProviderSettingsError> {
        if let Self::Ollama { api_base } = self {
            let url = Url::parse(api_base).map_err(ProviderSettingsError::InvalidOllamaUrl)?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(ProviderSettingsError::UnsafeOllamaUrl);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderSettingsFile {
    version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    profiles: BTreeMap<String, Vec<ConfiguredProvider>>,
}

#[derive(Debug)]
pub struct ProviderSettingsStore {
    path: PathBuf,
    providers: BTreeMap<String, Vec<ConfiguredProvider>>,
}

impl ProviderSettingsStore {
    pub fn default_path() -> Result<PathBuf, ProviderSettingsError> {
        dirs::config_dir()
            .map(|path| path.join("rynna").join("providers.toml"))
            .ok_or(ProviderSettingsError::ConfigDirectoryUnavailable)
    }

    pub fn load_default() -> Result<Self, ProviderSettingsError> {
        Self::load(Self::default_path()?)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ProviderSettingsError> {
        let path = path.as_ref().to_owned();
        let providers = read_provider_settings(&path)?;
        Ok(Self { path, providers })
    }

    pub fn refresh(&mut self) -> Result<(), ProviderSettingsError> {
        let _lock = self.lock_exclusive()?;
        self.providers = read_provider_settings(&self.path)?;
        Ok(())
    }

    pub fn mcp_settings_path(&self) -> PathBuf {
        self.path.with_file_name("mcp.toml")
    }

    pub fn memory_settings_path(&self) -> PathBuf {
        self.path.with_file_name("memory.toml")
    }

    pub fn list(&self, profile: &str) -> Vec<ConfiguredProvider> {
        self.providers.get(profile).cloned().unwrap_or_default()
    }

    pub fn get(&self, profile: &str, id: &str) -> Option<&ConfiguredProvider> {
        self.providers
            .get(profile)
            .and_then(|providers| providers.iter().find(|provider| provider.id() == id))
    }

    pub fn add(
        &mut self,
        profile: &str,
        provider: ConfiguredProvider,
    ) -> Result<(), ProviderSettingsError> {
        ensure_not_blank_provider_profile(profile)?;
        provider.validate()?;
        let _lock = self.lock_exclusive()?;
        let mut providers = read_provider_settings(&self.path)?;
        let profile_providers = providers.entry(profile.to_owned()).or_default();
        let id = provider.id();
        if profile_providers
            .iter()
            .any(|candidate| candidate.id() == id)
        {
            return Err(ProviderSettingsError::Duplicate(id.to_owned()));
        }
        profile_providers.push(provider);
        self.save(&providers)?;
        self.providers = providers;
        Ok(())
    }

    pub fn update(
        &mut self,
        profile: &str,
        provider: ConfiguredProvider,
    ) -> Result<(), ProviderSettingsError> {
        ensure_not_blank_provider_profile(profile)?;
        provider.validate()?;
        let _lock = self.lock_exclusive()?;
        let mut providers = read_provider_settings(&self.path)?;
        let id = provider.id();
        let existing = providers
            .get_mut(profile)
            .into_iter()
            .flatten()
            .find(|candidate| candidate.id() == id)
            .ok_or_else(|| ProviderSettingsError::NotConfigured(id.to_owned()))?;
        *existing = provider;
        self.save(&providers)?;
        self.providers = providers;
        Ok(())
    }

    pub fn delete(&mut self, profile: &str, id: &str) -> Result<(), ProviderSettingsError> {
        ensure_not_blank_provider_profile(profile)?;
        let _lock = self.lock_exclusive()?;
        let mut providers = read_provider_settings(&self.path)?;
        let profile_providers = providers
            .get_mut(profile)
            .ok_or_else(|| ProviderSettingsError::NotConfigured(id.to_owned()))?;
        let index = profile_providers
            .iter()
            .position(|provider| provider.id() == id)
            .ok_or_else(|| ProviderSettingsError::NotConfigured(id.to_owned()))?;
        profile_providers.remove(index);
        if profile_providers.is_empty() {
            providers.remove(profile);
        }
        self.save(&providers)?;
        self.providers = providers;
        Ok(())
    }

    pub fn rename_profile(
        &mut self,
        original_name: &str,
        new_name: &str,
    ) -> Result<(), ProviderSettingsError> {
        ensure_not_blank_provider_profile(original_name)?;
        ensure_not_blank_provider_profile(new_name)?;
        let _lock = self.lock_exclusive()?;
        let mut providers = read_provider_settings(&self.path)?;
        if original_name == new_name {
            self.providers = providers;
            return Ok(());
        }
        let Some(profile_providers) = providers.remove(original_name) else {
            self.providers = providers;
            return Ok(());
        };
        if providers.contains_key(new_name) {
            return Err(ProviderSettingsError::ProfileAlreadyConfigured(
                new_name.to_owned(),
            ));
        }
        providers.insert(new_name.to_owned(), profile_providers);
        self.save(&providers)?;
        self.providers = providers;
        Ok(())
    }

    pub fn delete_profile(&mut self, profile: &str) -> Result<(), ProviderSettingsError> {
        ensure_not_blank_provider_profile(profile)?;
        let _lock = self.lock_exclusive()?;
        let mut providers = read_provider_settings(&self.path)?;
        if providers.remove(profile).is_some() {
            self.save(&providers)?;
        }
        self.providers = providers;
        Ok(())
    }

    fn lock_exclusive(&self) -> Result<fs::File, ProviderSettingsError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| ProviderSettingsError::Write {
                path: self.path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "provider settings path has no parent directory",
                ),
            })?;
        fs::create_dir_all(parent).map_err(|source| ProviderSettingsError::Write {
            path: self.path.clone(),
            source,
        })?;
        let lock_path = self.path.with_extension("toml.lock");
        let mut options = fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&lock_path)
            .map_err(|source| ProviderSettingsError::Write {
                path: lock_path.clone(),
                source,
            })?;
        file.lock_exclusive()
            .map_err(|source| ProviderSettingsError::Write {
                path: lock_path,
                source,
            })?;
        Ok(file)
    }

    fn save(
        &self,
        providers: &BTreeMap<String, Vec<ConfiguredProvider>>,
    ) -> Result<(), ProviderSettingsError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| ProviderSettingsError::Write {
                path: self.path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "provider settings path has no parent directory",
                ),
            })?;
        fs::create_dir_all(parent).map_err(|source| ProviderSettingsError::Write {
            path: self.path.clone(),
            source,
        })?;
        let encoded = toml::to_string_pretty(&ProviderSettingsFile {
            version: PROVIDER_SETTINGS_VERSION,
            profiles: providers.clone(),
        })?;
        let temporary =
            write_private_temporary_file(&self.path, encoded.as_bytes()).map_err(|source| {
                ProviderSettingsError::Write {
                    path: self.path.clone(),
                    source,
                }
            })?;
        let _temporary_cleanup = TemporaryFileCleanup(temporary.clone());
        replace_file(&temporary, &self.path).map_err(|source| ProviderSettingsError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

fn read_provider_settings(
    path: &Path,
) -> Result<BTreeMap<String, Vec<ConfiguredProvider>>, ProviderSettingsError> {
    let providers = match path.try_exists() {
        Ok(false) => BTreeMap::new(),
        Ok(true) => {
            let source =
                fs::read_to_string(path).map_err(|source| ProviderSettingsError::Read {
                    path: path.to_owned(),
                    source,
                })?;
            let file: ProviderSettingsFile = toml::from_str(&source)?;
            if file.version != PROVIDER_SETTINGS_VERSION {
                return Err(ProviderSettingsError::UnsupportedVersion(file.version));
            }
            file.profiles
        }
        Err(source) => {
            return Err(ProviderSettingsError::Inspect {
                path: path.to_owned(),
                source,
            });
        }
    };
    for (profile, profile_providers) in &providers {
        ensure_not_blank_provider_profile(profile)?;
        let mut seen = BTreeSet::new();
        for provider in profile_providers {
            provider.validate()?;
            if !seen.insert(provider.id()) {
                return Err(ProviderSettingsError::Duplicate(provider.id().to_owned()));
            }
        }
    }
    Ok(providers)
}

fn ensure_not_blank_provider_profile(profile: &str) -> Result<(), ProviderSettingsError> {
    if profile.trim().is_empty() {
        return Err(ProviderSettingsError::BlankProfile);
    }
    Ok(())
}

struct TemporaryFileCleanup(PathBuf);

impl Drop for TemporaryFileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn write_private_temporary_file(destination: &Path, contents: &[u8]) -> std::io::Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider settings path has no parent directory",
        )
    })?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("providers.toml");
    for _ in 0..32 {
        let sequence = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match write_private_file(&temporary, contents) {
            Ok(()) => return Ok(temporary),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create a unique provider settings temporary file",
    ))
}

fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

pub fn secure_private_directory(path: impl Into<PathBuf>) -> std::io::Result<PathBuf> {
    let path = path.into();
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private directory must be an absolute path",
        ));
    }
    let mut normal_components = 0;
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "private directory must not contain parent-directory components",
                ));
            }
            Component::Normal(_) => normal_components += 1,
            _ => {}
        }
    }
    if normal_components == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a filesystem root cannot be used as a private directory",
        ));
    }
    #[cfg(unix)]
    {
        secure_private_directory_unix(path)
    }
    #[cfg(not(unix))]
    {
        secure_private_directory_portable(path)
    }
}

#[cfg(unix)]
fn secure_private_directory_unix(path: PathBuf) -> std::io::Result<PathBuf> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let root = CString::new("/").expect("the root path has no NUL bytes");
    // SAFETY: root is a valid NUL-terminated path and the returned descriptor is owned here.
    let root_fd = unsafe { libc::open(root.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: open returned a fresh descriptor on success.
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = CString::new(name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private directory path contains a NUL byte",
            )
        })?;
        // SAFETY: directory and name are valid; mkdirat does not follow the final component.
        if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: status points to writable storage and the descriptor/name are valid.
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fstatat initialized status after returning success.
        let status = unsafe { status.assume_init() };
        if status.st_mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(std::io::Error::other(
                "private directory must not be a symbolic link or contain symbolic links",
            ));
        }
        if status.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(std::io::Error::other(
                "private directory path must contain only directories",
            ));
        }
        // SAFETY: directory and name are valid. O_NOFOLLOW rejects a symbolic-link component.
        let child_fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        if child_fd < 0 {
            let error = std::io::Error::last_os_error();
            return if matches!(error.raw_os_error(), Some(libc::ELOOP)) {
                Err(std::io::Error::other(
                    "private directory must not be a symbolic link or contain symbolic links",
                ))
            } else {
                Err(error)
            };
        }
        // SAFETY: openat returned a fresh descriptor on success.
        directory = unsafe { OwnedFd::from_raw_fd(child_fd) };
    }
    // SAFETY: directory is a valid descriptor opened by this function.
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(path)
}

#[cfg(not(unix))]
fn secure_private_directory_portable(path: PathBuf) -> std::io::Result<PathBuf> {
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::other(
                "private directory must not be a symbolic link",
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(std::io::Error::other(
                "private directory must be a directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&path)?;
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(std::io::Error::other(
                    "private directory must be a non-symbolic directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    Ok(path)
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProviderSettingsError {
    #[error("provider settings are not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("provider settings could not be encoded: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("provider settings version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("provider `{0}` is already configured")]
    Duplicate(String),
    #[error("provider `{0}` is not configured")]
    NotConfigured(String),
    #[error("provider settings profile must not be blank")]
    BlankProfile,
    #[error("provider settings for profile `{0}` are already configured")]
    ProfileAlreadyConfigured(String),
    #[error("Ollama API base URL is invalid: {0}")]
    InvalidOllamaUrl(url::ParseError),
    #[error("Ollama API base URL must use HTTP or HTTPS and contain no credentials")]
    UnsafeOllamaUrl,
    #[error("failed to read provider settings at {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to inspect provider settings at {}: {source}", path.display())]
    Inspect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write provider settings at {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the platform configuration directory is unavailable")]
    ConfigDirectoryUnavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderKind {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "claude-subscription")]
    ClaudeSubscription,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProfile {
    /// Base for profile skill paths; relative to the catalog, never the UI client.
    pub skills_directory: PathBuf,
    pub profile: Profile,
    pub providers: Vec<ResolvedProvider>,
    pub system_prompt: String,
    pub capabilities: Vec<ResolvedCapability>,
}

impl ResolvedProfile {
    pub fn override_default_model(&mut self, model: &str) {
        if let Some(provider) = self.providers.first_mut() {
            if let Some(metadata) = self.profile.providers.iter_mut().find(|entry| {
                entry.enabled && entry.provider == provider.name && entry.model == provider.model
            }) {
                metadata.model = model.to_owned();
            }
            provider.model = model.to_owned();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProvider {
    pub name: String,
    pub model: String,
    pub provider_kind: ProviderKind,
    pub api_base: String,
    pub api_key_env: Option<String>,
    pub claude_program: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedCapability {
    FileSystem(FileSystemCapability),
    Command(CommandCapability),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandCapability {
    pub working_directory: PathBuf,
    pub programs: BTreeMap<String, PathBuf>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSystemCapability {
    pub root: PathBuf,
    pub read_only: bool,
    pub allowed_patterns: Vec<String>,
    pub denied_patterns: Option<Vec<String>>,
    pub protected_patterns: Option<Vec<String>>,
    pub max_read_bytes: Option<usize>,
    pub max_results: Option<usize>,
    pub max_traversal_files: Option<usize>,
    pub max_traversal_depth: Option<usize>,
    pub max_search_bytes: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ProfileCatalog {
    path: Option<PathBuf>,
    default_profile: String,
    providers: BTreeMap<String, ProviderConfig>,
    profiles: BTreeMap<String, ProfileConfig>,
    mcp_servers: BTreeMap<String, toml::Table>,
    capabilities: BTreeMap<String, CapabilityConfig>,
}

impl ProfileCatalog {
    pub fn built_in() -> Self {
        Self {
            path: None,
            default_profile: DEFAULT_PROFILE.to_owned(),
            providers: BTreeMap::from([(
                DEFAULT_PROVIDER.to_owned(),
                ProviderConfig {
                    kind: ProviderKind::OpenAiCompatible,
                    api_base: DEFAULT_API_BASE.to_owned(),
                    api_key_env: None,
                    claude_program: default_claude_program(),
                },
            )]),
            profiles: BTreeMap::from([(
                DEFAULT_PROFILE.to_owned(),
                ProfileConfig {
                    providers: vec![ProfileProvider {
                        provider: DEFAULT_PROVIDER.to_owned(),
                        model: DEFAULT_MODEL.to_owned(),
                        enabled: true,
                        is_default: true,
                    }],
                    provider: None,
                    model: None,
                    system_prompt: Some(DEFAULT_SYSTEM_PROMPT.to_owned()),
                    active_skills: Vec::new(),
                    mcp_servers: Vec::new(),
                    capabilities: Vec::new(),
                    default_workspace_directory: default_workspace_directory(),
                    workspaces: Vec::new(),
                },
            )]),
            mcp_servers: BTreeMap::new(),
            capabilities: BTreeMap::new(),
        }
    }

    pub fn from_toml(source: &str) -> Result<Self, ConfigError> {
        let file: ConfigFile = toml::from_str(source)?;
        Self::from_file(file)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut catalog = Self::from_toml(&source)?;
        catalog.path = Some(path.to_owned());
        Ok(catalog)
    }

    pub fn default_path() -> Result<PathBuf, ConfigError> {
        dirs::config_dir()
            .map(|path| path.join("rynna").join("config.toml"))
            .ok_or(ConfigError::ConfigDirectoryUnavailable)
    }

    pub fn load_default() -> Result<Self, ConfigError> {
        let path = Self::default_path()?;
        Self::load_default_from(&path)
    }

    fn load_default_from(path: &Path) -> Result<Self, ConfigError> {
        match path.try_exists() {
            Ok(true) => Self::load(path),
            Ok(false) => {
                let mut catalog = Self::built_in();
                catalog.path = Some(path.to_owned());
                Ok(catalog)
            }
            Err(source) => Err(ConfigError::Inspect {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub fn default_profile(&self) -> &str {
        &self.default_profile
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn add_profile(&mut self, profile: Profile) -> Result<Profile, ConfigError> {
        ensure_profile_is_mutable(&profile.name)?;
        let _lock = self.lock_exclusive()?;
        let file = self.fresh_file()?;
        if file.profiles.contains_key(&profile.name) {
            return Err(ConfigError::DuplicateProfile(profile.name));
        }
        self.write_profile(file, None, profile)
    }

    pub fn update_profile(
        &mut self,
        original_name: &str,
        profile: Profile,
    ) -> Result<Profile, ConfigError> {
        ensure_profile_is_mutable(original_name)?;
        ensure_profile_is_mutable(&profile.name)?;
        let _lock = self.lock_exclusive()?;
        let file = self.fresh_file()?;
        if !file.profiles.contains_key(original_name) {
            return Err(ConfigError::UnknownProfile(original_name.to_owned()));
        }
        if profile.name != original_name && file.profiles.contains_key(&profile.name) {
            return Err(ConfigError::DuplicateProfile(profile.name));
        }
        self.write_profile(file, Some(original_name), profile)
    }

    pub fn delete_profile(&mut self, name: &str) -> Result<(), ConfigError> {
        ensure_profile_is_mutable(name)?;
        let _lock = self.lock_exclusive()?;
        let mut file = self.fresh_file()?;
        if !file.profiles.contains_key(name) {
            return Err(ConfigError::UnknownProfile(name.to_owned()));
        }
        if file.profiles.len() <= 1 {
            return Err(ConfigError::LastProfile);
        }
        file.profiles.remove(name);
        if file.default_profile == name {
            file.default_profile = file
                .profiles
                .keys()
                .next()
                .cloned()
                .expect("a remaining profile exists");
        }
        self.apply_file(file)
    }

    fn write_profile(
        &mut self,
        mut file: ConfigFile,
        original_name: Option<&str>,
        mut profile: Profile,
    ) -> Result<Profile, ConfigError> {
        if !profile.providers.iter().any(|provider| provider.is_default)
            && let Some(provider) = profile
                .providers
                .iter_mut()
                .find(|provider| provider.enabled)
        {
            provider.is_default = true;
        }
        let system_prompt = original_name
            .and_then(|name| file.profiles.get(name))
            .and_then(|existing| existing.system_prompt.clone());
        if let Some(name) = original_name {
            file.profiles.remove(name);
            if file.default_profile == name {
                file.default_profile = profile.name.clone();
            }
        }
        file.profiles.insert(
            profile.name.clone(),
            ProfileConfig {
                providers: profile.providers.clone(),
                provider: None,
                model: None,
                system_prompt,
                active_skills: profile.active_skills.clone(),
                mcp_servers: profile.mcp_servers.clone(),
                capabilities: profile.capabilities.clone(),
                default_workspace_directory: profile.default_workspace_directory.clone(),
                workspaces: profile.workspaces.clone(),
            },
        );
        self.apply_file(file)?;
        Ok(profile)
    }

    fn fresh_file(&self) -> Result<ConfigFile, ConfigError> {
        let Some(path) = &self.path else {
            return Ok(self.to_file());
        };
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => Ok(Self::load(path)?.to_file()),
            Ok(_) => Ok(self.to_file()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(self.to_file()),
            Err(source) => Err(ConfigError::Inspect {
                path: path.clone(),
                source,
            }),
        }
    }

    fn lock_exclusive(&self) -> Result<Option<fs::File>, ConfigError> {
        let Some(path) = &self.path else {
            return Ok(None);
        };
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: path.clone(),
            source,
        })?;
        let lock_path = path.with_extension("toml.lock");
        let mut options = fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&lock_path)
            .map_err(|source| ConfigError::Write {
                path: lock_path.clone(),
                source,
            })?;
        file.lock_exclusive().map_err(|source| ConfigError::Write {
            path: lock_path,
            source,
        })?;
        Ok(Some(file))
    }

    fn apply_file(&mut self, file: ConfigFile) -> Result<(), ConfigError> {
        let path = self.path.clone();
        let mut updated = Self::from_file(file)?;
        updated.path = path;
        updated.save()?;
        *self = updated;
        Ok(())
    }

    fn to_file(&self) -> ConfigFile {
        ConfigFile {
            version: CONFIG_VERSION,
            default_profile: self.default_profile.clone(),
            providers: self.providers.clone(),
            profiles: self.profiles.clone(),
            mcp_servers: self.mcp_servers.clone(),
            capabilities: self.capabilities.clone(),
        }
    }

    fn save(&self) -> Result<(), ConfigError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().ok_or_else(|| ConfigError::Write {
            path: path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "configuration path has no parent directory",
            ),
        })?;
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: path.clone(),
            source,
        })?;
        let encoded = toml::to_string_pretty(&self.to_file())?;
        let temporary =
            write_private_temporary_file(path, encoded.as_bytes()).map_err(|source| {
                ConfigError::Write {
                    path: path.clone(),
                    source,
                }
            })?;
        let _temporary_cleanup = TemporaryFileCleanup(temporary.clone());
        replace_file(&temporary, path).map_err(|source| ConfigError::Write {
            path: path.clone(),
            source,
        })
    }

    pub fn resolve_all(&self) -> Result<Vec<ResolvedProfile>, ConfigError> {
        self.profiles
            .keys()
            .map(|name| self.resolve(name))
            .collect()
    }

    pub fn mcp_server(&self, name: &str) -> Option<&toml::Table> {
        self.mcp_servers.get(name)
    }

    pub fn resolve(&self, name: &str) -> Result<ResolvedProfile, ConfigError> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| ConfigError::UnknownProfile(name.to_owned()))?;
        let mut enabled_providers = profile
            .providers
            .iter()
            .filter(|entry| entry.enabled)
            .collect::<Vec<_>>();
        enabled_providers.sort_by_key(|entry| !entry.is_default);
        let providers = enabled_providers
            .into_iter()
            .map(|entry| {
                let provider = self.providers.get(&entry.provider).ok_or_else(|| {
                    ConfigError::UnknownProvider {
                        profile: name.to_owned(),
                        provider: entry.provider.clone(),
                    }
                })?;
                Ok(ResolvedProvider {
                    name: entry.provider.clone(),
                    model: entry.model.clone(),
                    provider_kind: provider.kind,
                    api_base: provider.api_base.clone(),
                    api_key_env: provider.api_key_env.clone(),
                    claude_program: provider.claude_program.clone(),
                })
            })
            .collect::<Result<Vec<_>, ConfigError>>()?;

        Ok(ResolvedProfile {
            skills_directory: self
                .path
                .as_deref()
                .and_then(Path::parent)
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_owned(),
            profile: Profile {
                name: name.to_owned(),
                providers: profile.providers.clone(),
                active_skills: profile.active_skills.clone(),
                mcp_servers: profile.mcp_servers.clone(),
                capabilities: profile.capabilities.clone(),
                default_workspace_directory: profile.default_workspace_directory.clone(),
                workspaces: profile.workspaces.clone(),
            },
            providers,
            system_prompt: profile
                .system_prompt
                .clone()
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_owned()),
            capabilities: profile
                .capabilities
                .iter()
                .map(|name| match &self.capabilities[name] {
                    CapabilityConfig::FileSystem(config) => {
                        ResolvedCapability::FileSystem(config.clone().into())
                    }
                    CapabilityConfig::Command(config) => {
                        ResolvedCapability::Command(config.clone().into())
                    }
                })
                .collect(),
        })
    }

    fn from_file(mut file: ConfigFile) -> Result<Self, ConfigError> {
        if file.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion(file.version));
        }
        for profile in file.profiles.values_mut() {
            if profile.providers.is_empty()
                && let (Some(provider), Some(model)) =
                    (profile.provider.take(), profile.model.take())
            {
                profile.providers.push(ProfileProvider {
                    provider,
                    model,
                    enabled: true,
                    is_default: true,
                });
            }
            if !profile.providers.iter().any(|provider| provider.is_default)
                && let Some(provider) = profile
                    .providers
                    .iter_mut()
                    .find(|provider| provider.enabled)
            {
                provider.is_default = true;
            }
        }
        if !file.profiles.contains_key(&file.default_profile) {
            return Err(ConfigError::UnknownDefault(file.default_profile));
        }

        for (name, provider) in &file.providers {
            ensure_not_blank("provider name", name)?;
            if provider.kind == ProviderKind::ClaudeSubscription {
                if !provider.api_base.is_empty() || provider.api_key_env.is_some() {
                    return Err(ConfigError::ClaudeSubscriptionApiConfiguration {
                        provider: name.clone(),
                    });
                }
                ensure_not_blank(
                    "Claude program",
                    provider.claude_program.to_string_lossy().as_ref(),
                )?;
            } else {
                ensure_not_blank("provider API base URL", &provider.api_base)?;
                let api_base = Url::parse(&provider.api_base).map_err(|source| {
                    ConfigError::InvalidProviderUrl {
                        provider: name.clone(),
                        source,
                    }
                })?;
                if !api_base.username().is_empty() || api_base.password().is_some() {
                    return Err(ConfigError::EmbeddedProviderCredentials {
                        provider: name.clone(),
                    });
                }
            }
            if let Some(api_key_env) = &provider.api_key_env {
                ensure_not_blank("provider API key environment variable", api_key_env)?;
            }
        }

        for (name, capability) in &file.capabilities {
            ensure_not_blank("capability name", name)?;
            if let CapabilityConfig::Command(command) = capability {
                if command.programs.is_empty() {
                    return Err(ConfigError::CommandProgramsEmpty {
                        capability: name.clone(),
                    });
                }
                if command.timeout_seconds == 0
                    || command.max_output_bytes == 0
                    || command.timeout_seconds > MAX_COMMAND_TIMEOUT_SECONDS
                    || command.max_output_bytes > MAX_COMMAND_OUTPUT_BYTES
                {
                    return Err(ConfigError::InvalidCommandLimit {
                        capability: name.clone(),
                    });
                }
                for (alias, path) in &command.programs {
                    ensure_not_blank("command program alias", alias)?;
                    if !path.is_absolute() {
                        return Err(ConfigError::CommandProgramNotAbsolute {
                            capability: name.clone(),
                            alias: alias.clone(),
                        });
                    }
                }
            }
        }

        for (name, profile) in &file.profiles {
            ensure_not_blank("profile name", name)?;
            if profile.providers.is_empty() {
                return Err(ConfigError::BlankValue("profile providers"));
            }
            if !profile.providers.iter().any(|provider| provider.enabled) {
                return Err(ConfigError::NoEnabledModels {
                    profile: name.clone(),
                });
            }
            if profile
                .providers
                .iter()
                .filter(|provider| provider.is_default)
                .count()
                != 1
                || profile
                    .providers
                    .iter()
                    .any(|provider| provider.is_default && !provider.enabled)
            {
                return Err(ConfigError::InvalidDefaultModels {
                    profile: name.clone(),
                });
            }
            let mut seen_models = BTreeSet::new();
            for provider in &profile.providers {
                ensure_not_blank("profile provider", &provider.provider)?;
                ensure_not_blank("profile model", &provider.model)?;
                if !seen_models.insert((&provider.provider, &provider.model)) {
                    return Err(ConfigError::DuplicateProfileModel {
                        profile: name.clone(),
                        provider: provider.provider.clone(),
                        model: provider.model.clone(),
                    });
                }
                if !file.providers.contains_key(&provider.provider) {
                    return Err(ConfigError::UnknownProvider {
                        profile: name.clone(),
                        provider: provider.provider.clone(),
                    });
                }
            }
            if profile.providers.iter().any(|provider| {
                file.providers[&provider.provider].kind == ProviderKind::ClaudeSubscription
            }) && (!profile.active_skills.is_empty()
                || !profile.mcp_servers.is_empty()
                || !profile.capabilities.is_empty())
            {
                return Err(ConfigError::ClaudeSubscriptionContext {
                    profile: name.clone(),
                });
            }
            ensure_unique("active skill", &profile.active_skills)?;
            ensure_unique("MCP server", &profile.mcp_servers)?;
            ensure_unique("capability", &profile.capabilities)?;
            ensure_not_blank(
                "default workspace directory",
                profile
                    .default_workspace_directory
                    .to_string_lossy()
                    .as_ref(),
            )?;
            let mut workspace_names = BTreeSet::new();
            for workspace in &profile.workspaces {
                ensure_not_blank("workspace name", &workspace.name)?;
                if !workspace_names.insert(&workspace.name) {
                    return Err(ConfigError::DuplicateWorkspace {
                        profile: name.clone(),
                        workspace: workspace.name.clone(),
                    });
                }
                if workspace.directories.is_empty() {
                    return Err(ConfigError::WorkspaceDirectoriesEmpty {
                        profile: name.clone(),
                        workspace: workspace.name.clone(),
                    });
                }
                let mut directories = BTreeSet::new();
                for directory in &workspace.directories {
                    ensure_not_blank("workspace directory", directory.to_string_lossy().as_ref())?;
                    if !directories.insert(directory) {
                        return Err(ConfigError::DuplicateWorkspaceDirectory {
                            profile: name.clone(),
                            workspace: workspace.name.clone(),
                            directory: directory.clone(),
                        });
                    }
                }
                if !directories.contains(&workspace.default_directory) {
                    return Err(ConfigError::UnknownWorkspaceDefaultDirectory {
                        profile: name.clone(),
                        workspace: workspace.name.clone(),
                    });
                }
            }
            for server in &profile.mcp_servers {
                if !file.mcp_servers.contains_key(server) {
                    return Err(ConfigError::UnknownMcpServer {
                        profile: name.clone(),
                        server: server.clone(),
                    });
                }
            }
            for capability in &profile.capabilities {
                if !file.capabilities.contains_key(capability) {
                    return Err(ConfigError::UnknownCapability {
                        profile: name.clone(),
                        capability: capability.clone(),
                    });
                }
            }
            let filesystem_capabilities = profile
                .capabilities
                .iter()
                .filter(|capability| {
                    matches!(
                        file.capabilities.get(*capability),
                        Some(CapabilityConfig::FileSystem(_))
                    )
                })
                .count();
            if filesystem_capabilities > 1 {
                return Err(ConfigError::ConflictingFileSystemCapabilities {
                    profile: name.clone(),
                });
            }
            let command_capabilities = profile
                .capabilities
                .iter()
                .filter(|capability| {
                    matches!(
                        file.capabilities.get(*capability),
                        Some(CapabilityConfig::Command(_))
                    )
                })
                .count();
            if command_capabilities > 1 {
                return Err(ConfigError::ConflictingCommandCapabilities {
                    profile: name.clone(),
                });
            }
        }

        Ok(Self {
            path: None,
            default_profile: file.default_profile,
            providers: file.providers,
            profiles: file.profiles,
            mcp_servers: file.mcp_servers,
            capabilities: file.capabilities,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    default_profile: String,
    providers: BTreeMap<String, ProviderConfig>,
    profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    mcp_servers: BTreeMap<String, toml::Table>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    capabilities: BTreeMap<String, CapabilityConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    kind: ProviderKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    api_base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_env: Option<String>,
    #[serde(default = "default_claude_program")]
    claude_program: PathBuf,
}

fn default_claude_program() -> PathBuf {
    PathBuf::from("claude")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileConfig {
    #[serde(default)]
    providers: Vec<ProfileProvider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    active_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mcp_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    capabilities: Vec<String>,
    #[serde(default = "default_workspace_directory")]
    default_workspace_directory: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    workspaces: Vec<Workspace>,
}

fn default_workspace_directory() -> PathBuf {
    PathBuf::from(".")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum CapabilityConfig {
    #[serde(rename = "filesystem")]
    FileSystem(FileSystemCapabilityConfig),
    #[serde(rename = "command")]
    Command(CommandCapabilityConfig),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommandCapabilityConfig {
    working_directory: PathBuf,
    programs: BTreeMap<String, PathBuf>,
    timeout_seconds: u64,
    max_output_bytes: usize,
}

impl From<CommandCapabilityConfig> for CommandCapability {
    fn from(config: CommandCapabilityConfig) -> Self {
        Self {
            working_directory: config.working_directory,
            programs: config.programs,
            timeout_seconds: config.timeout_seconds,
            max_output_bytes: config.max_output_bytes,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileSystemCapabilityConfig {
    root: PathBuf,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    allowed_patterns: Vec<String>,
    denied_patterns: Option<Vec<String>>,
    protected_patterns: Option<Vec<String>>,
    max_read_bytes: Option<usize>,
    max_results: Option<usize>,
    max_traversal_files: Option<usize>,
    max_traversal_depth: Option<usize>,
    max_search_bytes: Option<usize>,
}

impl From<FileSystemCapabilityConfig> for FileSystemCapability {
    fn from(config: FileSystemCapabilityConfig) -> Self {
        Self {
            root: config.root,
            read_only: config.read_only,
            allowed_patterns: config.allowed_patterns,
            denied_patterns: config.denied_patterns,
            protected_patterns: config.protected_patterns,
            max_read_bytes: config.max_read_bytes,
            max_results: config.max_results,
            max_traversal_files: config.max_traversal_files,
            max_traversal_depth: config.max_traversal_depth,
            max_search_bytes: config.max_search_bytes,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("profile `{0}` is defined more than once")]
    DuplicateProfile(String),
    #[error("the last profile cannot be deleted")]
    LastProfile,
    #[error("profile name `{0}` is reserved for a runtime-only profile")]
    ReservedProfile(String),
    #[error("Rynna configuration is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Rynna configuration version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("default profile `{0}` is not defined")]
    UnknownDefault(String),
    #[error("profile `{0}` is not defined")]
    UnknownProfile(String),
    #[error("profile `{profile}` references unknown provider `{provider}`")]
    UnknownProvider { profile: String, provider: String },
    #[error("profile `{profile}` must have at least one enabled model")]
    NoEnabledModels { profile: String },
    #[error("profile `{profile}` must have exactly one default model, and it must be enabled")]
    InvalidDefaultModels { profile: String },
    #[error("profile `{profile}` contains duplicate model `{provider}/{model}`")]
    DuplicateProfileModel {
        profile: String,
        provider: String,
        model: String,
    },
    #[error("provider `{provider}` base URL is invalid: {source}")]
    InvalidProviderUrl {
        provider: String,
        source: url::ParseError,
    },
    #[error("provider `{provider}` base URL must not contain embedded credentials")]
    EmbeddedProviderCredentials { provider: String },
    #[error("profile `{profile}` references unknown MCP server `{server}`")]
    UnknownMcpServer { profile: String, server: String },
    #[error("profile `{profile}` references unknown capability `{capability}`")]
    UnknownCapability { profile: String, capability: String },
    #[error(
        "profile `{profile}` activates multiple filesystem capabilities, whose tool names would conflict"
    )]
    ConflictingFileSystemCapabilities { profile: String },
    #[error(
        "profile `{profile}` activates multiple command capabilities, whose tool names would conflict"
    )]
    ConflictingCommandCapabilities { profile: String },
    #[error("profile `{profile}` defines workspace `{workspace}` more than once")]
    DuplicateWorkspace { profile: String, workspace: String },
    #[error("workspace `{workspace}` in profile `{profile}` must contain at least one directory")]
    WorkspaceDirectoriesEmpty { profile: String, workspace: String },
    #[error("workspace `{workspace}` in profile `{profile}` contains duplicate directory `{}`", directory.display())]
    DuplicateWorkspaceDirectory {
        profile: String,
        workspace: String,
        directory: PathBuf,
    },
    #[error(
        "the default directory for workspace `{workspace}` in profile `{profile}` must be one of its directories"
    )]
    UnknownWorkspaceDefaultDirectory { profile: String, workspace: String },
    #[error(
        "Claude subscription profile `{profile}` cannot declare skills, MCP servers, or capabilities"
    )]
    ClaudeSubscriptionContext { profile: String },
    #[error(
        "Claude subscription provider `{provider}` cannot declare an API base URL or API key environment variable"
    )]
    ClaudeSubscriptionApiConfiguration { provider: String },
    #[error("command capability `{capability}` must configure at least one program")]
    CommandProgramsEmpty { capability: String },
    #[error(
        "command capability `{capability}` timeout and output limits must be greater than zero and within the safe maximum"
    )]
    InvalidCommandLimit { capability: String },
    #[error("command capability `{capability}` program `{alias}` must use an absolute path")]
    CommandProgramNotAbsolute { capability: String, alias: String },
    #[error("{0} must not be blank")]
    BlankValue(&'static str),
    #[error("{kind} `{name}` is listed more than once")]
    DuplicateReference { kind: &'static str, name: String },
    #[error("failed to read Rynna configuration at {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to inspect Rynna configuration path {}: {source}", path.display())]
    Inspect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write Rynna configuration at {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to encode Rynna configuration: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("the platform configuration directory is unavailable")]
    ConfigDirectoryUnavailable,
}

fn ensure_profile_is_mutable(name: &str) -> Result<(), ConfigError> {
    if name == OPENAI_ACCOUNT_PROFILE {
        return Err(ConfigError::ReservedProfile(name.to_owned()));
    }
    Ok(())
}

fn ensure_not_blank(kind: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::BlankValue(kind));
    }
    Ok(())
}

fn ensure_unique(kind: &'static str, values: &[String]) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for value in values {
        ensure_not_blank(kind, value)?;
        if !seen.insert(value) {
            return Err(ConfigError::DuplicateReference {
                kind,
                name: value.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::ProfileCatalog;

    #[test]
    fn default_loading_propagates_filesystem_lookup_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        symlink("config.toml", &path).unwrap();

        assert!(ProfileCatalog::load_default_from(&path).is_err());
    }
}
