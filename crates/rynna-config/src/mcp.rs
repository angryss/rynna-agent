use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpSettings {
    #[serde(rename = "mcpServers")]
    pub servers: BTreeMap<String, McpServer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpServer {
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Remote tool name to expose in place of the built-in code_search tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_search_tool: Option<String>,
    #[serde(flatten)]
    pub transport: McpTransport,
}
fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_token_env: Option<String>,
    },
}

#[derive(Debug, Error)]
pub enum McpSettingsError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("could not read MCP settings")]
    Read,
    #[error("could not save MCP settings")]
    Write,
}

impl McpSettings {
    pub fn validate(&self) -> Result<(), McpSettingsError> {
        let invalid = McpSettingsError::Invalid;
        if self.servers.len() > 32 {
            return Err(invalid("configure at most 32 MCP servers per profile"));
        }
        if self
            .servers
            .values()
            .filter(|server| server.enabled && server.code_search_tool.is_some())
            .count()
            > 1
        {
            return Err(invalid(
                "select at most one enabled code search provider per profile",
            ));
        }
        for (name, server) in &self.servers {
            if server
                .code_search_tool
                .as_ref()
                .is_some_and(|name| name.trim().is_empty() || name.contains('\0'))
            {
                return Err(invalid("enter a nonempty remote code search tool name"));
            }
            if name.is_empty()
                || name.len() > 40
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return Err(invalid(
                    "MCP server names must be 1–40 letters, digits, underscores or hyphens",
                ));
            }
            match &server.transport {
                McpTransport::Stdio { command, args, env } => {
                    if command.trim().is_empty()
                        || command.contains('\0')
                        || args.iter().any(|v| v.contains('\0'))
                    {
                        return Err(invalid("enter an MCP command and valid arguments"));
                    }
                    if env
                        .iter()
                        .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
                    {
                        return Err(invalid("enter valid environment variable names and values"));
                    }
                }
                McpTransport::StreamableHttp {
                    url,
                    bearer_token_env,
                } => {
                    let parsed = url::Url::parse(url)
                        .map_err(|_| invalid("enter an absolute HTTP(S) MCP URL"))?;
                    if !matches!(parsed.scheme(), "http" | "https")
                        || parsed.host_str().is_none()
                        || !parsed.username().is_empty()
                        || parsed.password().is_some()
                        || parsed.fragment().is_some()
                    {
                        return Err(invalid(
                            "enter an HTTP(S) MCP URL without embedded credentials or a fragment",
                        ));
                    }
                    if bearer_token_env
                        .as_ref()
                        .is_some_and(|v| v.is_empty() || v.contains(['=', '\0']))
                    {
                        return Err(invalid(
                            "enter a valid bearer token environment variable name",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct McpSettingsFile {
    version: u32,
    #[serde(default)]
    // Decode only the selected profile so invalid entries remain independently repairable.
    profiles: BTreeMap<String, serde_yaml_ng::Value>,
}

pub struct McpSettingsStore {
    path: PathBuf,
}

impl McpSettingsStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_owned(),
        }
    }

    pub(super) fn read_profiles(
        &self,
    ) -> Result<BTreeMap<String, serde_yaml_ng::Value>, McpSettingsError> {
        let source = match std::fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                super::ensure_no_legacy_configuration(&self.path).map_err(|_| {
                    McpSettingsError::Invalid("legacy TOML configuration found; convert it to YAML at the requested settings path (see README migration instructions)")
                })?;
                return Ok(BTreeMap::new());
            }
            Err(_) => return Err(McpSettingsError::Read),
        };
        let file: McpSettingsFile = super::parse_yaml(&source).map_err(|_| {
            McpSettingsError::Invalid(
                "mcp.yaml must use version: 1 and a profiles mapping keyed by profile name",
            )
        })?;
        if file.version != 1 {
            return Err(McpSettingsError::Invalid(
                "unsupported MCP settings version",
            ));
        }
        for profile in file.profiles.keys() {
            validate_profile(profile)?;
        }
        Ok(file.profiles)
    }

    pub fn load(&self, profile: &str) -> Result<McpSettings, McpSettingsError> {
        validate_profile(profile)?;
        match self.read_profiles()?.remove(profile) {
            Some(value) => decode_settings(value),
            None => Ok(McpSettings::default()),
        }
    }

    pub(super) fn lock(&self) -> Result<std::fs::File, McpSettingsError> {
        super::ProviderSettingsStore {
            path: self.path.clone(),
            providers: Default::default(),
        }
        .lock_exclusive()
        .map_err(|_| McpSettingsError::Write)
    }

    fn write_profiles(
        &self,
        profiles: BTreeMap<String, serde_yaml_ng::Value>,
    ) -> Result<(), McpSettingsError> {
        let encoded = serde_yaml_ng::to_string(&McpSettingsFile {
            version: 1,
            profiles,
        })
        .map_err(|_| McpSettingsError::Write)?;
        let temporary = super::write_private_temporary_file(&self.path, encoded.as_bytes())
            .map_err(|_| McpSettingsError::Write)?;
        let _cleanup = super::TemporaryFileCleanup(temporary.clone());
        super::replace_file(&temporary, &self.path).map_err(|_| McpSettingsError::Write)
    }

    pub fn save(
        &self,
        profile: &str,
        settings: McpSettings,
    ) -> Result<McpSettings, McpSettingsError> {
        validate_profile(profile)?;
        let _lock = self.lock()?;
        let mut profiles = self.read_profiles()?;
        settings.validate()?;
        profiles.insert(
            profile.to_owned(),
            serde_yaml_ng::to_value(&settings).map_err(|_| McpSettingsError::Write)?,
        );
        self.write_profiles(profiles)?;
        Ok(settings)
    }

    pub fn rename_profile(&self, original: &str, name: &str) -> Result<(), McpSettingsError> {
        validate_profile(original)?;
        validate_profile(name)?;
        if original == name {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut profiles = self.read_profiles()?;
        if profiles.contains_key(name) {
            return Err(McpSettingsError::Invalid(
                "MCP settings already exist for the new profile name",
            ));
        }
        if let Some(settings) = profiles.remove(original) {
            profiles.insert(name.to_owned(), settings);
            self.write_profiles(profiles)?;
        }
        Ok(())
    }

    pub fn delete_profile(&self, profile: &str) -> Result<(), McpSettingsError> {
        validate_profile(profile)?;
        let _lock = self.lock()?;
        let mut profiles = self.read_profiles()?;
        if profiles.remove(profile).is_some() {
            self.write_profiles(profiles)?;
        }
        Ok(())
    }
}

fn validate_profile(profile: &str) -> Result<(), McpSettingsError> {
    if profile.trim().is_empty() {
        return Err(McpSettingsError::Invalid("MCP profile must not be blank"));
    }
    Ok(())
}

fn decode_settings(value: serde_yaml_ng::Value) -> Result<McpSettings, McpSettingsError> {
    let settings: McpSettings = serde_yaml_ng::from_value(value)
        .map_err(|_| McpSettingsError::Invalid("invalid MCP server configuration"))?;
    settings.validate()?;
    Ok(settings)
}
