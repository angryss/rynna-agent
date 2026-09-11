//! Bounded Codex app-server protocol shared by desktop and server adapters.
use rynna_config::secure_private_directory;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};
use tokio::time::{Instant, timeout};
pub const MAX_CODEX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct OpenAiCredentialSelection(Arc<AtomicBool>);

impl OpenAiCredentialSelection {
    pub fn new(reuse_existing: bool) -> Self {
        Self(Arc::new(AtomicBool::new(reuse_existing)))
    }

    pub fn reuses_existing(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn set_reuse_existing(&self, reuse_existing: bool) {
        self.0.store(reuse_existing, Ordering::Release);
    }
}

pub async fn write_codex_message(
    writer: &mut (impl AsyncWriteExt + Unpin),
    message: &serde_json::Value,
    deadline: Instant,
) -> Result<(), String> {
    let mut encoded = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    if encoded.len() > MAX_CODEX_MESSAGE_BYTES {
        return Err("Codex app-server request exceeded the size limit".to_owned());
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| "Codex app-server request timed out".to_owned())?;
    timeout(remaining, writer.write_all(&encoded))
        .await
        .map_err(|_| "Codex app-server request timed out".to_owned())?
        .map_err(|error| format!("failed to write to Codex app-server: {error}"))
}

pub async fn read_codex_response(
    reader: &mut (impl AsyncBufRead + Unpin),
    id: u64,
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    loop {
        let message = read_codex_message(reader, deadline).await?;
        if message.get("id").and_then(|value| value.as_u64()) == Some(id) {
            if let Some(error) = message
                .pointer("/error/message")
                .and_then(|value| value.as_str())
            {
                return Err(format!("Codex app-server request failed: {error}"));
            }
            return Ok(message);
        }
    }
}

pub async fn read_codex_message(
    reader: &mut (impl AsyncBufRead + Unpin),
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    let mut line = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "Codex app-server response timed out".to_owned())?;
        let available = timeout(remaining, reader.fill_buf())
            .await
            .map_err(|_| "Codex app-server response timed out".to_owned())?
            .map_err(|error| format!("failed to read Codex app-server response: {error}"))?;
        if available.is_empty() {
            if line.is_empty() {
                return Err("Codex app-server stopped unexpectedly".to_owned());
            }
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if line.len().saturating_add(take) > MAX_CODEX_MESSAGE_BYTES {
            return Err("Codex app-server message exceeded the size limit".to_owned());
        }
        let complete = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if complete {
            break;
        }
    }
    serde_json::from_slice(&line).map_err(|_| "Codex app-server returned invalid JSON".to_owned())
}

pub fn secure_codex_home(home: PathBuf) -> Result<PathBuf, String> {
    secure_private_directory(home).map_err(|error| {
        if error.to_string().contains("symbolic link") {
            "Rynna's Codex directory must not be a symbolic link or contain symbolic links"
                .to_owned()
        } else {
            format!("failed to prepare Rynna's Codex directory: {error}")
        }
    })
}
