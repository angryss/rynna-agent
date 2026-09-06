use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
#[cfg(unix)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper, KillOnDrop, ProcessGroup};
use rynna_core::{Tool, ToolDefinition, ToolError};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;
#[cfg(unix)]
use tokio::sync::oneshot;

const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 32 * 1024;
#[cfg(unix)]
const MAX_EXECUTABLE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TIMEOUT_SECONDS: u64 = 300;
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct CommandConfig {
    pub working_directory: PathBuf,
    pub programs: BTreeMap<String, PathBuf>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
}

pub struct CommandTool {
    workflow_policy: String,
    aliases: Vec<String>,
    #[cfg(unix)]
    working_directory: Arc<File>,
    #[cfg(unix)]
    programs: BTreeMap<String, PathBuf>,
    #[cfg(unix)]
    _program_directory: tempfile::TempDir,
    timeout: Duration,
    max_output_bytes: usize,
}

impl CommandTool {
    pub fn new(config: CommandConfig) -> Result<Self, CommandConfigError> {
        if config.timeout_seconds == 0
            || config.max_output_bytes == 0
            || config.timeout_seconds > MAX_TIMEOUT_SECONDS
            || config.max_output_bytes > MAX_OUTPUT_BYTES
        {
            return Err(CommandConfigError::InvalidLimit {
                max_timeout_seconds: MAX_TIMEOUT_SECONDS,
                max_output_bytes: MAX_OUTPUT_BYTES,
            });
        }
        if config.programs.is_empty() {
            return Err(CommandConfigError::NoPrograms);
        }

        #[cfg(not(unix))]
        {
            let _ = config;
            return Err(CommandConfigError::UnsupportedPlatform);
        }

        #[cfg(unix)]
        {
            let workflow_policy =
                format!("{config:?}:{:?}", config.working_directory.canonicalize());
            let working_directory = open_working_directory(&config.working_directory)?;
            let program_directory = tempfile::Builder::new()
                .prefix("rynna-command-")
                .tempdir()
                .map_err(CommandConfigError::ProgramDirectory)?;
            let mut aliases = Vec::with_capacity(config.programs.len());
            let mut programs = BTreeMap::new();
            for (index, (alias, path)) in config.programs.into_iter().enumerate() {
                if alias.trim().is_empty() {
                    return Err(CommandConfigError::BlankAlias);
                }
                if !path.is_absolute() {
                    return Err(CommandConfigError::ProgramNotAbsolute { alias, path });
                }
                let executable = prepare_executable(
                    &alias,
                    &path,
                    &program_directory.path().join(format!("program-{index}")),
                )?;
                aliases.push(alias.clone());
                programs.insert(alias, executable);
            }

            Ok(Self {
                aliases,
                workflow_policy,
                working_directory: Arc::new(working_directory),
                programs,
                _program_directory: program_directory,
                timeout: Duration::from_secs(config.timeout_seconds),
                max_output_bytes: config.max_output_bytes,
            })
        }
    }
}

#[cfg(unix)]
fn open_working_directory(path: &PathBuf) -> Result<File, CommandConfigError> {
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| CommandConfigError::WorkingDirectory {
            path: path.clone(),
            source,
        })?;
    if !directory
        .metadata()
        .map_err(|source| CommandConfigError::WorkingDirectory {
            path: path.clone(),
            source,
        })?
        .is_dir()
    {
        return Err(CommandConfigError::NotDirectory(path.clone()));
    }
    Ok(directory)
}

#[cfg(unix)]
fn prepare_executable(
    alias: &str,
    path: &PathBuf,
    private_path: &PathBuf,
) -> Result<PathBuf, CommandConfigError> {
    let mut executable = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| CommandConfigError::Program {
            alias: alias.to_owned(),
            path: path.clone(),
            source,
        })?;
    let metadata = executable
        .metadata()
        .map_err(|source| CommandConfigError::Program {
            alias: alias.to_owned(),
            path: path.clone(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(CommandConfigError::ProgramNotFile {
            alias: alias.to_owned(),
            path: path.clone(),
        });
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(CommandConfigError::ProgramNotExecutable {
            alias: alias.to_owned(),
            path: path.clone(),
        });
    }
    // macOS platform executables may be terminated when run from a copied inode.
    // The read-only root volume already protects their canonical paths from replacement.
    #[cfg(target_os = "macos")]
    if metadata.len() <= MAX_EXECUTABLE_BYTES
        && let Some(system_path) = readonly_system_executable(&executable, path)
    {
        return Ok(system_path);
    }
    let mut private = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(private_path)
        .map_err(CommandConfigError::ProgramCopy)?;
    let copied = std::io::copy(
        &mut std::io::Read::take(&mut executable, MAX_EXECUTABLE_BYTES + 1),
        &mut private,
    )
    .map_err(CommandConfigError::ProgramCopy)?;
    if copied > MAX_EXECUTABLE_BYTES {
        return Err(CommandConfigError::ProgramTooLarge {
            alias: alias.to_owned(),
            max_bytes: MAX_EXECUTABLE_BYTES,
        });
    }
    private
        .set_permissions(std::fs::Permissions::from_mode(0o500))
        .map_err(CommandConfigError::ProgramCopy)?;
    Ok(private_path.clone())
}

#[cfg(target_os = "macos")]
fn readonly_system_executable(executable: &File, path: &std::path::Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: executable retains a valid descriptor and filesystem has space for statfs.
    if unsafe { libc::fstatfs(executable.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: a successful fstatfs initialized the output structure.
    let filesystem = unsafe { filesystem.assume_init() };
    let required = (libc::MNT_RDONLY | libc::MNT_ROOTFS) as u32;
    if filesystem.f_flags & required != required {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    let opened = executable.metadata().ok()?;
    let named = canonical.metadata().ok()?;
    // A configured symlink may be mutable; retain only its immutable resolved target,
    // and fail closed if resolution raced with replacement of that symlink.
    if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
        return None;
    }
    Some(canonical)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandArguments {
    program: String,
    #[serde(default)]
    arguments: Vec<String>,
}

#[async_trait]
impl Tool for CommandTool {
    fn workflow_policy(&self) -> String {
        self.workflow_policy.clone()
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "run_command",
            format!(
                "Run one configured program directly without a shell. Available program aliases: {}",
                self.aliases.join(", ")
            ),
            json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string", "enum": self.aliases},
                    "arguments": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": MAX_ARGUMENTS
                    }
                },
                "required": ["program"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let arguments: CommandArguments = serde_json::from_value(arguments)
            .map_err(|error| ToolError::new(format!("invalid run_command arguments: {error}")))?;
        if arguments.arguments.len() > MAX_ARGUMENTS {
            return Err(ToolError::new(format!(
                "run_command accepts at most {MAX_ARGUMENTS} arguments"
            )));
        }
        let argument_bytes = arguments.arguments.iter().map(String::len).sum::<usize>();
        if argument_bytes > MAX_ARGUMENT_BYTES {
            return Err(ToolError::new(format!(
                "run_command arguments exceed the {MAX_ARGUMENT_BYTES}-byte limit"
            )));
        }

        #[cfg(not(unix))]
        {
            let _ = arguments;
            Err(ToolError::new(
                "run_command is not supported on this platform",
            ))
        }

        #[cfg(unix)]
        self.execute_unix(arguments).await
    }
}

#[cfg(unix)]
impl CommandTool {
    async fn execute_unix(
        &self,
        arguments: CommandArguments,
    ) -> Result<serde_json::Value, ToolError> {
        let executable = self.programs.get(&arguments.program).ok_or_else(|| {
            ToolError::new(format!(
                "program alias `{}` is not allowed by command policy",
                arguments.program
            ))
        })?;
        let working_directory = self.working_directory.try_clone().map_err(|error| {
            ToolError::new(format!("failed to prepare working directory: {error}"))
        })?;
        let mut command = CommandWrap::with_new(executable, |command| {
            command
                .args(&arguments.arguments)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        command
            .wrap(ProcessGroup::leader())
            .wrap(WorkingDirectory {
                working_directory: Some(working_directory),
            })
            .wrap(KillOnDrop);
        let mut child = command
            .spawn()
            .map_err(|error| ToolError::new(format!("failed to start program: {error}")))?;
        let stdout = match child.stdout().take() {
            Some(stdout) => stdout,
            None => {
                return Err(terminate_after_error(
                    child.as_mut(),
                    ToolError::new("failed to capture program stdout"),
                )
                .await);
            }
        };
        let stderr = match child.stderr().take() {
            Some(stderr) => stderr,
            None => {
                return Err(terminate_after_error(
                    child.as_mut(),
                    ToolError::new("failed to capture program stderr"),
                )
                .await);
            }
        };
        let timeout = self.timeout;
        let max_output_bytes = self.max_output_bytes;
        let (cancel_sender, mut cancel_receiver) = oneshot::channel();
        let mut cancellation = ExecutionCancellation::new(cancel_sender);
        let supervisor = tokio::spawn(async move {
            let budget = Arc::new(OutputBudget::new(max_output_bytes));
            let outcome = {
                let execution = async {
                    let ((stdout, stderr), status) = tokio::try_join!(
                        read_combined(stdout, stderr, Arc::clone(&budget)),
                        async {
                            child.wait().await.map_err(|error| {
                                ToolError::new(format!("failed to wait for program group: {error}"))
                            })
                        }
                    )?;
                    Ok::<_, ToolError>((stdout, stderr, status))
                };
                tokio::pin!(execution);
                tokio::select! {
                    result = &mut execution => ExecutionOutcome::Completed(result),
                    _ = tokio::time::sleep(timeout) => ExecutionOutcome::TimedOut,
                    _ = &mut cancel_receiver => ExecutionOutcome::Cancelled,
                }
            };
            match outcome {
                ExecutionOutcome::Completed(Ok(result)) => Ok(result),
                ExecutionOutcome::Completed(Err(error)) => {
                    Err(terminate_after_error(child.as_mut(), error).await)
                }
                ExecutionOutcome::TimedOut => Err(terminate_after_error(
                    child.as_mut(),
                    ToolError::new(format!(
                        "program exceeded the {}-second timeout",
                        timeout.as_secs()
                    )),
                )
                .await),
                ExecutionOutcome::Cancelled => Err(terminate_after_error(
                    child.as_mut(),
                    ToolError::new("program execution was cancelled"),
                )
                .await),
            }
        });
        let supervised = supervisor
            .await
            .map_err(|error| ToolError::new(format!("program supervisor failed: {error}")))?;
        cancellation.disarm();
        let (stdout, stderr, status) = supervised?;
        let stdout = String::from_utf8(stdout)
            .map_err(|_| ToolError::new("program stdout is not valid UTF-8"))?;
        let stderr = String::from_utf8(stderr)
            .map_err(|_| ToolError::new("program stderr is not valid UTF-8"))?;

        Ok(json!({
            "success": status.success(),
            "exit_code": status.code(),
            "stdout": stdout,
            "stderr": stderr
        }))
    }
}

#[cfg(unix)]
enum ExecutionOutcome<T> {
    Completed(T),
    TimedOut,
    Cancelled,
}

#[cfg(unix)]
struct ExecutionCancellation(Option<oneshot::Sender<()>>);

#[cfg(unix)]
impl ExecutionCancellation {
    fn new(sender: oneshot::Sender<()>) -> Self {
        Self(Some(sender))
    }

    fn disarm(&mut self) {
        self.0.take();
    }
}

#[cfg(unix)]
impl Drop for ExecutionCancellation {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct WorkingDirectory {
    working_directory: Option<File>,
}

#[cfg(unix)]
impl CommandWrapper for WorkingDirectory {
    fn pre_spawn(
        &mut self,
        command: &mut tokio::process::Command,
        _core: &CommandWrap,
    ) -> std::io::Result<()> {
        let working_directory = self.working_directory.take().ok_or_else(|| {
            std::io::Error::other("working-directory handle was already consumed")
        })?;
        // SAFETY: fchdir is async-signal-safe, and the retained descriptor identifies the
        // authorized working directory opened when the capability was composed.
        unsafe {
            command.pre_exec(move || {
                if libc::fchdir(working_directory.as_raw_fd()) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

struct OutputBudget {
    limit: usize,
    state: Mutex<OutputBudgetState>,
    changed: Notify,
}

struct OutputBudgetState {
    remaining: usize,
    in_flight: usize,
    consumed: usize,
}

impl OutputBudget {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            state: Mutex::new(OutputBudgetState {
                remaining: limit + 1,
                in_flight: 0,
                consumed: 0,
            }),
            changed: Notify::new(),
        }
    }

    async fn reserve(&self) -> Result<usize, ToolError> {
        loop {
            let notified = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| ToolError::new("program output budget lock was poisoned"))?;
                if state.remaining > 0 {
                    let granted = state.remaining.min(4096);
                    state.remaining -= granted;
                    state.in_flight += granted;
                    return Ok(granted);
                }
                if state.in_flight == 0 {
                    return Err(self.exceeded());
                }
            }
            notified.await;
        }
    }

    fn complete(&self, granted: usize, read: usize) -> Result<(), ToolError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::new("program output budget lock was poisoned"))?;
        state.in_flight -= granted;
        state.remaining += granted - read;
        state.consumed += read;
        let exceeded = state.consumed > self.limit;
        drop(state);
        self.changed.notify_waiters();
        if exceeded {
            Err(self.exceeded())
        } else {
            Ok(())
        }
    }

    fn exceeded(&self) -> ToolError {
        ToolError::new(format!(
            "program output exceeds the {}-byte limit",
            self.limit
        ))
    }
}

async fn read_combined(
    mut stdout: impl AsyncRead + Unpin,
    mut stderr: impl AsyncRead + Unpin,
    budget: Arc<OutputBudget>,
) -> Result<(Vec<u8>, Vec<u8>), ToolError> {
    let mut stdout_output = Vec::new();
    let mut stderr_output = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;
    loop {
        if stdout_done && stderr_done {
            return Ok((stdout_output, stderr_output));
        }
        let granted = budget.reserve().await?;
        let mut stdout_buffer = vec![0_u8; granted];
        let mut stderr_buffer = vec![0_u8; granted];
        let (is_stdout, read) = match (stdout_done, stderr_done) {
            (false, false) => tokio::select! {
                read = stdout.read(&mut stdout_buffer) => (true, read),
                read = stderr.read(&mut stderr_buffer) => (false, read),
            },
            (false, true) => (true, stdout.read(&mut stdout_buffer).await),
            (true, false) => (false, stderr.read(&mut stderr_buffer).await),
            (true, true) => unreachable!(),
        };
        let read = read
            .map_err(|error| ToolError::new(format!("failed to read program output: {error}")))?;
        budget.complete(granted, read)?;
        if read == 0 {
            if is_stdout {
                stdout_done = true;
            } else {
                stderr_done = true;
            }
        } else if is_stdout {
            stdout_output.extend_from_slice(&stdout_buffer[..read]);
        } else {
            stderr_output.extend_from_slice(&stderr_buffer[..read]);
        }
    }
}

#[cfg(unix)]
async fn terminate(child: &mut dyn ChildWrapper) -> Result<(), ToolError> {
    let mut failures = Vec::new();
    if let Err(error) = child.try_wait() {
        failures.push(format!("process-group status check failed: {error}"));
    }
    match child.start_kill() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
        Err(error) => failures.push(format!("process-group signal failed: {error}")),
    }
    if let Err(error) = child.wait().await {
        failures.push(format!("direct-child reap failed: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ToolError::new(failures.join("; ")))
    }
}

#[cfg(unix)]
async fn terminate_after_error(child: &mut dyn ChildWrapper, error: ToolError) -> ToolError {
    match terminate(child).await {
        Ok(()) => error,
        Err(cleanup) => {
            eprintln!("rynna command cleanup failure: {cleanup}");
            ToolError::new(format!("{error}; {cleanup}"))
        }
    }
}

#[derive(Debug, Error)]
pub enum CommandConfigError {
    #[error(
        "command timeout and output limits must be greater than zero and no greater than {max_timeout_seconds} seconds and {max_output_bytes} bytes"
    )]
    InvalidLimit {
        max_timeout_seconds: u64,
        max_output_bytes: usize,
    },
    #[error("command capability must configure at least one program")]
    NoPrograms,
    #[error("command program aliases must not be blank")]
    BlankAlias,
    #[error("command working directory is not a directory: {}", .0.display())]
    NotDirectory(PathBuf),
    #[error("failed to inspect command working directory {}: {source}", path.display())]
    WorkingDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("command program `{alias}` must use an absolute path, not {}", path.display())]
    ProgramNotAbsolute { alias: String, path: PathBuf },
    #[error("failed to inspect command program `{alias}` at {}: {source}", path.display())]
    Program {
        alias: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("command program `{alias}` is not a regular file: {}", path.display())]
    ProgramNotFile { alias: String, path: PathBuf },
    #[error("command program `{alias}` is not executable: {}", path.display())]
    ProgramNotExecutable { alias: String, path: PathBuf },
    #[error("command program `{alias}` exceeds the {max_bytes}-byte executable limit")]
    ProgramTooLarge { alias: String, max_bytes: u64 },
    #[error("failed to create a private command program directory: {0}")]
    ProgramDirectory(std::io::Error),
    #[error("failed to create a private command program copy: {0}")]
    ProgramCopy(std::io::Error),
    #[error("command capabilities are currently supported only on Unix platforms")]
    UnsupportedPlatform,
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::os::unix::process::ExitStatusExt;
    use std::pin::Pin;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, ReadBuf};

    use process_wrap::tokio::ChildWrapper;

    use super::{OutputBudget, read_combined, terminate};

    #[derive(Debug)]
    struct FailingChild;

    impl ChildWrapper for FailingChild {
        fn inner(&self) -> &dyn ChildWrapper {
            panic!("terminal test child has no inner child")
        }

        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            panic!("terminal test child has no inner child")
        }

        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            panic!("terminal test child has no inner child")
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("cleanup failed"))
        }

        fn wait(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<ExitStatus>> + Send + '_>> {
            Box::pin(async { Ok(ExitStatus::from_raw(0)) })
        }

        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            Ok(Some(ExitStatus::from_raw(0)))
        }
    }

    #[derive(Debug, Default)]
    struct StatusCheckFailingChild {
        signal_calls: usize,
        wait_calls: usize,
    }

    impl ChildWrapper for StatusCheckFailingChild {
        fn inner(&self) -> &dyn ChildWrapper {
            panic!("terminal test child has no inner child")
        }

        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            panic!("terminal test child has no inner child")
        }

        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            panic!("terminal test child has no inner child")
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            self.signal_calls += 1;
            Ok(())
        }

        fn wait(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<ExitStatus>> + Send + '_>> {
            self.wait_calls += 1;
            Box::pin(async { Ok(ExitStatus::from_raw(0)) })
        }

        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
        }
    }

    struct CountingReader {
        remaining: usize,
        bytes_read: Arc<AtomicUsize>,
    }

    impl AsyncRead for CountingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let read = self.remaining.min(buffer.remaining());
            buffer.put_slice(&vec![b'x'; read]);
            self.remaining -= read;
            self.bytes_read.fetch_add(read, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    struct PendingReader;

    impl AsyncRead for PendingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn output_reader_never_reads_more_than_limit_plus_one() {
        let bytes_read = Arc::new(AtomicUsize::new(0));
        let budget = Arc::new(OutputBudget::new(9));
        let error = read_combined(
            PendingReader,
            CountingReader {
                remaining: 4096,
                bytes_read: Arc::clone(&bytes_read),
            },
            budget,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("9-byte limit"));
        assert!(bytes_read.load(Ordering::SeqCst) <= 10);
    }

    #[tokio::test]
    async fn concurrently_ready_streams_share_the_actual_read_budget() {
        let bytes_read = Arc::new(AtomicUsize::new(0));
        let budget = Arc::new(OutputBudget::new(9));
        let error = read_combined(
            CountingReader {
                remaining: 4096,
                bytes_read: Arc::clone(&bytes_read),
            },
            CountingReader {
                remaining: 4096,
                bytes_read: Arc::clone(&bytes_read),
            },
            budget,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("9-byte limit"));
        assert!(bytes_read.load(Ordering::SeqCst) <= 10);
    }

    #[tokio::test]
    async fn termination_reports_group_cleanup_failures() {
        let error = terminate(&mut FailingChild).await.unwrap_err();

        assert!(error.to_string().contains("cleanup failed"));
    }

    #[tokio::test]
    async fn termination_signals_and_reaps_after_status_check_failure() {
        let mut child = StatusCheckFailingChild::default();

        let error = terminate(&mut child).await.unwrap_err();

        assert!(error.to_string().contains("status check failed"));
        assert_eq!(child.signal_calls, 1);
        assert_eq!(child.wait_calls, 1);
    }
}
