//! Profile-specific memory settings. Credentials never appear in the response type.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use url::Url;

pub const HINDSIGHT_CLOUD_URL: &str = "https://api.hindsight.vectorize.io";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HindsightDeployment {
    Cloud,
    SelfHosted,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemorySettings {
    #[default]
    None,
    Hindsight {
        deployment: HindsightDeployment,
        api_base: String,
        bank_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemorySettingsResponse {
    None,
    Hindsight {
        deployment: HindsightDeployment,
        api_base: String,
        bank_id: String,
        api_key_configured: bool,
    },
}

#[derive(Debug, Error)]
pub enum MemorySettingsError {
    #[error("{0}")]
    Invalid(&'static str),
    // Parsing errors can contain credential-bearing source lines. Keep them private.
    #[error("could not read memory settings")]
    Read,
    #[error("could not save memory settings")]
    Write,
}

impl MemorySettings {
    pub fn response(&self) -> MemorySettingsResponse {
        match self {
            Self::None => MemorySettingsResponse::None,
            Self::Hindsight {
                deployment,
                api_base,
                bank_id,
                api_key,
            } => MemorySettingsResponse::Hindsight {
                deployment: *deployment,
                api_base: api_base.clone(),
                bank_id: bank_id.clone(),
                api_key_configured: api_key.as_ref().is_some_and(|key| !key.is_empty()),
            },
        }
    }

    pub fn validate(&self) -> Result<(), MemorySettingsError> {
        if let Self::Hindsight {
            deployment,
            api_base,
            bank_id,
            api_key,
        } = self
        {
            let invalid_url = || {
                MemorySettingsError::Invalid(
                    "enter an HTTP(S) API URL without credentials, query, or fragment",
                )
            };
            let url = Url::parse(api_base).map_err(|_| invalid_url())?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid_url());
            }
            if *deployment == HindsightDeployment::Cloud
                && api_base.trim_end_matches('/') != HINDSIGHT_CLOUD_URL
            {
                return Err(MemorySettingsError::Invalid(
                    "Hindsight Cloud must use its official API URL",
                ));
            }
            if bank_id.trim().is_empty()
                || bank_id.len() > 256
                || bank_id != bank_id.trim()
                || bank_id.chars().any(char::is_control)
                || matches!(bank_id.as_str(), "." | "..")
            {
                return Err(MemorySettingsError::Invalid(
                    "enter a valid memory bank ID (up to 256 bytes)",
                ));
            }
            if api_key.as_ref().is_some_and(|key| {
                key.len() > 16 * 1024 || !key.is_ascii() || key.chars().any(char::is_control)
            }) {
                return Err(MemorySettingsError::Invalid("enter a valid API key"));
            }
            if *deployment == HindsightDeployment::Cloud
                && api_key.as_ref().is_none_or(|key| key.trim().is_empty())
            {
                return Err(MemorySettingsError::Invalid(
                    "an API key is required for Hindsight Cloud",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MemorySettingsFile {
    version: u32,
    #[serde(default)]
    // Decode only the selected profile so invalid entries remain independently repairable.
    profiles: BTreeMap<String, serde_yaml_ng::Value>,
}

pub struct MemorySettingsStore {
    path: PathBuf,
}

impl MemorySettingsStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_owned(),
        }
    }

    pub(super) fn read_profiles(
        &self,
    ) -> Result<BTreeMap<String, serde_yaml_ng::Value>, MemorySettingsError> {
        let source = match std::fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                super::ensure_no_legacy_configuration(&self.path).map_err(|_| {
                    MemorySettingsError::Invalid("legacy TOML configuration found; convert it to YAML at the requested settings path (see README migration instructions)")
                })?;
                return Ok(BTreeMap::new());
            }
            Err(_) => return Err(MemorySettingsError::Read),
        };
        let file: MemorySettingsFile = super::parse_yaml(&source).map_err(|_| MemorySettingsError::Invalid(
            "memory.yaml must use version: 1 and a profiles mapping keyed by profile name; assign any previous global settings to a profile"
        ))?;
        if file.version != 1 {
            return Err(MemorySettingsError::Invalid(
                "unsupported memory settings version",
            ));
        }
        for profile in file.profiles.keys() {
            validate_profile(profile)?;
        }
        Ok(file.profiles)
    }

    pub fn load(&self, profile: &str) -> Result<MemorySettings, MemorySettingsError> {
        validate_profile(profile)?;
        match self.read_profiles()?.remove(profile) {
            Some(value) => decode_settings(value),
            None => Ok(MemorySettings::None),
        }
    }

    pub(super) fn lock(&self) -> Result<std::fs::File, MemorySettingsError> {
        super::ProviderSettingsStore {
            path: self.path.clone(),
            providers: Default::default(),
        }
        .lock_exclusive()
        .map_err(|_| MemorySettingsError::Write)
    }

    fn write_profiles(
        &self,
        profiles: BTreeMap<String, serde_yaml_ng::Value>,
    ) -> Result<(), MemorySettingsError> {
        let encoded = serde_yaml_ng::to_string(&MemorySettingsFile {
            version: 1,
            profiles,
        })
        .map_err(|_| MemorySettingsError::Write)?;
        let temporary = super::write_private_temporary_file(&self.path, encoded.as_bytes())
            .map_err(|_| MemorySettingsError::Write)?;
        let _cleanup = super::TemporaryFileCleanup(temporary.clone());
        super::replace_file(&temporary, &self.path).map_err(|_| MemorySettingsError::Write)
    }

    pub fn save(
        &self,
        profile: &str,
        mut settings: MemorySettings,
    ) -> Result<MemorySettings, MemorySettingsError> {
        validate_profile(profile)?;
        let _lock = self.lock()?;
        let mut profiles = self.read_profiles()?;
        // Omission preserves only this profile's credential for the same destination.
        if let MemorySettings::Hindsight {
            deployment,
            api_base,
            api_key,
            ..
        } = &mut settings
        {
            if api_key.is_none()
                && let Some(MemorySettings::Hindsight {
                    deployment: old_deployment,
                    api_base: old_base,
                    api_key: old_key,
                    ..
                }) = profiles
                    .get(profile)
                    .cloned()
                    .and_then(|value| decode_settings(value).ok())
                && *deployment == old_deployment
                && api_base.trim_end_matches('/') == old_base.trim_end_matches('/')
            {
                *api_key = old_key;
            }
            if api_key.as_ref().is_some_and(|key| key.is_empty()) {
                *api_key = None;
            }
        }
        settings.validate()?;
        profiles.insert(
            profile.to_owned(),
            serde_yaml_ng::to_value(&settings).map_err(|_| MemorySettingsError::Write)?,
        );
        self.write_profiles(profiles)?;
        Ok(settings)
    }

    pub fn rename_profile(&self, original: &str, name: &str) -> Result<(), MemorySettingsError> {
        validate_profile(original)?;
        validate_profile(name)?;
        if original == name {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut profiles = self.read_profiles()?;
        if profiles.contains_key(name) {
            return Err(MemorySettingsError::Invalid(
                "memory settings already exist for the new profile name",
            ));
        }
        if let Some(settings) = profiles.remove(original) {
            profiles.insert(name.to_owned(), settings);
            self.write_profiles(profiles)?;
        }
        Ok(())
    }

    pub fn delete_profile(&self, profile: &str) -> Result<(), MemorySettingsError> {
        validate_profile(profile)?;
        let _lock = self.lock()?;
        let mut profiles = self.read_profiles()?;
        if profiles.remove(profile).is_some() {
            self.write_profiles(profiles)?;
        }
        Ok(())
    }
}

fn validate_profile(profile: &str) -> Result<(), MemorySettingsError> {
    if profile.trim().is_empty() {
        return Err(MemorySettingsError::Invalid(
            "memory profile must not be blank",
        ));
    }
    Ok(())
}

fn decode_settings(value: serde_yaml_ng::Value) -> Result<MemorySettings, MemorySettingsError> {
    let settings: MemorySettings = serde_yaml_ng::from_value(value).map_err(|_| {
        MemorySettingsError::Invalid(
            "invalid memory settings for this profile; save replacement settings",
        )
    })?;
    settings.validate()?;
    Ok(settings)
}
