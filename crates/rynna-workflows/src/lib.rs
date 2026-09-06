//! Private single-owner filesystem checkpoints. Invalid records fail closed.
use async_trait::async_trait;
use fs2::FileExt;
use rynna_core::workflow_runs::{Run, RunStore};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
pub struct FileRunStore {
    path: PathBuf,
    _owner: File,
}
impl FileRunStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_owned();
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("workflow store must not be a symlink".into());
        }
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let owner = options
            .open(path.join("owner.lock"))
            .map_err(|e| e.to_string())?;
        owner
            .try_lock_exclusive()
            .map_err(|_| "workflow store already has an owner".to_owned())?;
        Ok(Self {
            path,
            _owner: owner,
        })
    }
}
#[async_trait]
impl RunStore for FileRunStore {
    async fn load(&self) -> Result<Vec<Run>, String> {
        let mut runs = vec![];
        for entry in fs::read_dir(&self.path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let valid = entry.file_type().map_err(|e| e.to_string())?.is_file()
                && entry.metadata().map_err(|e| e.to_string())?.len() <= 4 * 1024 * 1024;
            let run = if valid {
                fs::read(entry.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Run>(&b).ok())
            } else {
                None
            };
            match run {
                Some(run)
                    if run.version == 1
                        && entry.file_name().to_string_lossy() == format!("{}.json", run.id)
                        && run.cursor < run.workflow.steps.len()
                        && run.workflow.validate(&run.helpers).is_ok()
                        && run.start.limits.validate().is_ok()
                        && rynna_core::workflow_runs::validate_criteria(&run.start.criteria)
                            .is_ok()
                        && !run.start.goal.trim().is_empty()
                        && run.start.goal.len() <= 8192
                        && run.start.initial_context.len() <= 16000
                        && run.workflow.id == run.start.workflow_id
                        && run.events.len() <= 50
                        && run.events.iter().all(|e| e.content.len() <= 32000)
                        && run.steering.len() <= 32
                        && run.consumed.steps <= run.start.limits.steps
                        && run.consumed.tool_calls <= run.start.limits.tool_calls
                        && run.consumed.active_seconds <= run.start.limits.active_seconds =>
                {
                    runs.push(run)
                }
                _ => {
                    return Err(format!(
                        "workflow record {} is corrupt or unsupported; store unavailable until repaired (record preserved)",
                        entry.file_name().to_string_lossy()
                    ));
                }
            }
        }
        Ok(runs)
    }
    async fn save(&self, run: &Run) -> Result<(), String> {
        let bytes = serde_json::to_vec(run).map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("run record exceeds 4 MiB".into());
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.path).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(self.path.join(format!("{}.json", run.id)))
            .map_err(|e| e.to_string())?;
        File::open(&self.path)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
}

pub mod host;
