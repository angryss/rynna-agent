//! Coordinate catalog renames with all profile-owned settings.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use rynna_core::Profile;
use serde::Serialize;
use thiserror::Error;

use crate::mcp::{McpSettingsError, McpSettingsStore};
use crate::memory::{MemorySettingsError, MemorySettingsStore};
use crate::{
    ConfigError, ProfileCatalog, ProviderSettingsError, ProviderSettingsFile,
    ProviderSettingsStore, TemporaryFileCleanup, read_provider_settings, replace_file,
    write_private_temporary_file,
};

#[derive(Debug, Error)]
pub enum ProfileUpdateError {
    #[error(transparent)]
    Catalog(#[from] ConfigError),
    #[error(transparent)]
    Providers(#[from] ProviderSettingsError),
    #[error(transparent)]
    Mcp(#[from] McpSettingsError),
    #[error(transparent)]
    Memory(#[from] MemorySettingsError),
    #[error("profile settings already exist for the new profile name")]
    DestinationExists,
    #[error("could not persist profile rename; original settings were preserved")]
    Persistence,
    #[error(
        "could not restore profile settings after a failed rename; private temporary recovery files were retained"
    )]
    Rollback,
}

#[derive(Serialize)]
struct SettingsFile {
    version: u32,
    profiles: BTreeMap<String, serde_yaml_ng::Value>,
}

/// Stage every file before replacing any, and restore completed replacements on error.
/// The file locks serialize cooperating writers; this is not a crash-recovery journal.
pub fn update_profile_with_settings(
    catalog: &mut ProfileCatalog,
    providers: &mut ProviderSettingsStore,
    original: &str,
    profile: Profile,
) -> Result<Profile, ProfileUpdateError> {
    if original == profile.name {
        return Ok(catalog.update_profile(original, profile)?);
    }
    let _catalog_lock = catalog.lock_exclusive()?;
    let _providers_lock = providers.lock_exclusive()?;
    let memory_path = providers.memory_settings_path();
    let mcp_path = providers.mcp_settings_path();
    let memory = MemorySettingsStore::new(&memory_path);
    let mcp = McpSettingsStore::new(&mcp_path);
    let _memory_lock = memory.lock()?;
    let _mcp_lock = mcp.lock()?;

    // A detached catalog validates the full edit without persisting it or changing runtime state.
    let mut staged_catalog = ProfileCatalog::from_file(catalog.fresh_file()?)?;
    let saved = staged_catalog.update_profile(original, profile)?;
    let mut staged_providers = read_provider_settings(&providers.path)?;
    let mut memory_profiles = memory.read_profiles()?;
    let mut mcp_profiles = mcp.read_profiles()?;
    let providers_changed = move_entry(&mut staged_providers, original, &saved.name)?;
    let memory_changed = move_entry(&mut memory_profiles, original, &saved.name)?;
    let mcp_changed = move_entry(&mut mcp_profiles, original, &saved.name)?;

    let mut writes = Vec::new();
    if providers_changed {
        writes.push(StagedFile::prepare(
            &providers.path,
            &ProviderSettingsFile {
                version: crate::PROVIDER_SETTINGS_VERSION,
                profiles: staged_providers.clone(),
            },
        )?);
    }
    if memory_changed {
        writes.push(StagedFile::prepare(
            &memory_path,
            &SettingsFile {
                version: 1,
                profiles: memory_profiles,
            },
        )?);
    }
    if mcp_changed {
        writes.push(StagedFile::prepare(
            &mcp_path,
            &SettingsFile {
                version: 1,
                profiles: mcp_profiles,
            },
        )?);
    }
    if let Some(path) = &catalog.path {
        writes.push(StagedFile::prepare(path, &staged_catalog.to_file())?);
    }
    commit(&mut writes)?;
    staged_catalog.path = catalog.path.clone();
    *catalog = staged_catalog;
    providers.providers = staged_providers;
    Ok(saved)
}

/// Persist provider settings and account model registration as one rollback-capable edit.
pub fn save_provider_with_catalog(
    catalog: &mut ProfileCatalog,
    providers: &mut ProviderSettingsStore,
    profile: &str,
    provider: crate::ConfiguredProvider,
    replace: bool,
) -> Result<(), ProfileUpdateError> {
    if !matches!(provider, crate::ConfiguredProvider::OpenAi { .. }) {
        catalog.resolve(profile)?;
        if replace {
            providers.update(profile, provider)?;
        } else {
            providers.add(profile, provider)?;
        }
        return Ok(());
    }
    let _catalog_lock = catalog.lock_exclusive()?;
    let _providers_lock = providers.lock_exclusive()?;
    let mut staged_catalog = ProfileCatalog::from_file(catalog.fresh_file()?)?;
    staged_catalog.resolve(profile)?;
    provider.validate()?;
    let mut staged_providers = ProviderSettingsStore {
        path: providers.path.clone(),
        providers: read_provider_settings(&providers.path)?,
    };
    let entries = staged_providers
        .providers
        .entry(profile.to_owned())
        .or_default();
    let existing = entries.iter().position(|entry| entry.id() == provider.id());
    match (replace, existing) {
        (false, Some(_)) => {
            return Err(ProviderSettingsError::Duplicate(provider.id().to_owned()).into());
        }
        (true, None) => {
            return Err(ProviderSettingsError::NotConfigured(provider.id().to_owned()).into());
        }
        (true, Some(index)) => entries[index] = provider,
        (false, None) => entries.push(provider),
    }
    staged_catalog.register_openai_accounts(&staged_providers)?;
    let mut writes = vec![StagedFile::prepare(
        &providers.path,
        &ProviderSettingsFile {
            version: crate::PROVIDER_SETTINGS_VERSION,
            profiles: staged_providers.providers.clone(),
        },
    )?];
    if let Some(path) = &catalog.path {
        writes.push(StagedFile::prepare(path, &staged_catalog.to_file())?);
    }
    commit(&mut writes)?;
    staged_catalog.path = catalog.path.clone();
    *catalog = staged_catalog;
    providers.providers = staged_providers.providers;
    Ok(())
}

fn move_entry<T>(
    entries: &mut BTreeMap<String, T>,
    original: &str,
    name: &str,
) -> Result<bool, ProfileUpdateError> {
    if entries.contains_key(name) {
        return Err(ProfileUpdateError::DestinationExists);
    }
    if let Some(value) = entries.remove(original) {
        entries.insert(name.to_owned(), value);
        Ok(true)
    } else {
        Ok(false)
    }
}

struct StagedFile {
    path: PathBuf,
    replacement: TemporaryFileCleanup,
    backup: Option<TemporaryFileCleanup>,
}
impl StagedFile {
    fn prepare(path: &Path, value: &impl Serialize) -> Result<Self, ProfileUpdateError> {
        let original = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(ProfileUpdateError::Persistence),
        };
        let encoded =
            serde_yaml_ng::to_string(value).map_err(|_| ProfileUpdateError::Persistence)?;
        let replacement = TemporaryFileCleanup(
            write_private_temporary_file(path, encoded.as_bytes())
                .map_err(|_| ProfileUpdateError::Persistence)?,
        );
        let backup = original
            .map(|bytes| write_private_temporary_file(path, &bytes).map(TemporaryFileCleanup))
            .transpose()
            .map_err(|_| ProfileUpdateError::Persistence)?;
        Ok(Self {
            path: path.to_owned(),
            replacement,
            backup,
        })
    }
}

fn commit(writes: &mut [StagedFile]) -> Result<(), ProfileUpdateError> {
    for index in 0..writes.len() {
        if replace_file(&writes[index].replacement.0, &writes[index].path).is_err() {
            let mut restored = true;
            for write in writes[..index].iter_mut().rev() {
                let result = match &write.backup {
                    Some(backup) => replace_file(&backup.0, &write.path),
                    None => fs::remove_file(&write.path),
                };
                if result.is_err() {
                    restored = false;
                    // Keep the owner-only recovery copy if the filesystem also rejects rollback.
                    if let Some(backup) = write.backup.take() {
                        std::mem::forget(backup);
                    }
                }
            }
            return Err(if restored {
                ProfileUpdateError::Persistence
            } else {
                ProfileUpdateError::Rollback
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_replacement_failure_restores_earlier_files_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.yaml");
        let last = dir.path().join("last.yaml");
        fs::write(&first, "# original formatting\nvalue: 'old'\n").unwrap();
        fs::write(&last, "value: old\n").unwrap();
        let before = fs::read(&first).unwrap();
        let value = BTreeMap::from([("value", "new")]);
        let mut writes = vec![
            StagedFile::prepare(&first, &value).unwrap(),
            StagedFile::prepare(&last, &value).unwrap(),
        ];
        // Force a commit-time error after all staging/preflight has succeeded.
        fs::remove_file(&last).unwrap();
        fs::create_dir(&last).unwrap();
        assert!(matches!(
            commit(&mut writes),
            Err(ProfileUpdateError::Persistence)
        ));
        assert_eq!(fs::read(&first).unwrap(), before);
    }
}
