#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use rynna_core::Tool;
use rynna_tools_command::{CommandConfig, CommandTool};
use serde_json::json;

// CommandTool forks before exec to set its working directory. A parallel test
// can fork while executable() holds a writable fixture descriptor, inheriting
// it until exec even with CLOEXEC. Linux then rejects that fixture with ETXTBSY.
// Serialize fixture creation and execution in this test process; locking only
// executable() would still allow another test to fork during its write.
static FIXTURE_PROCESS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn executable(directory: &tempfile::TempDir, name: &str, source: &str) -> std::path::PathBuf {
    let program = directory.path().join(name);
    let mut fixture = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
    fixture.write_all(source.as_bytes()).unwrap();
    fixture
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .unwrap();
    drop(fixture.persist(&program).unwrap());
    program
}

fn fifo(directory: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = directory.path().join(name);
    let path_bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
    let c_path = std::ffi::CString::new(path_bytes).unwrap();
    // SAFETY: c_path is a valid NUL-terminated pathname and mode is permission bits only.
    let result = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(
        result,
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );
    path
}

fn config(
    directory: &tempfile::TempDir,
    alias: &str,
    program: std::path::PathBuf,
) -> CommandConfig {
    CommandConfig {
        working_directory: directory.path().to_owned(),
        programs: BTreeMap::from([(alias.to_owned(), program)]),
        timeout_seconds: 5,
        max_output_bytes: 8192,
    }
}

#[tokio::test]
async fn runs_an_explicitly_mapped_program_without_a_shell() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(
        &directory,
        "show-args",
        "#!/bin/sh\nprintf 'system=%s\\n' \"$1\"\nprintf 'warning\\n' >&2\n",
    );
    let tool = CommandTool::new(config(&directory, "inspect", program)).unwrap();

    let result = tool
        .execute(json!({"program": "inspect", "arguments": ["Rynna OS"]}))
        .await
        .unwrap();

    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["success"], true);
    assert_eq!(result["stdout"], "system=Rynna OS\n");
    assert_eq!(result["stderr"], "warning\n");
}

#[tokio::test]
async fn rejects_unmapped_programs_without_executing_them() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("marker");
    let program = executable(
        &directory,
        "touch-marker",
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    let tool = CommandTool::new(config(&directory, "allowed", program)).unwrap();

    let error = tool
        .execute(json!({"program": "not-allowed"}))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("not allowed"));
    assert!(!marker.exists());
}

#[tokio::test]
async fn terminates_commands_that_exceed_the_timeout() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(&directory, "sleep", "#!/bin/sh\nsleep 10\n");
    let mut command_config = config(&directory, "sleep", program);
    command_config.timeout_seconds = 1;
    let tool = CommandTool::new(command_config).unwrap();

    let error = tool.execute(json!({"program": "sleep"})).await.unwrap_err();

    assert!(error.to_string().contains("1-second timeout"), "{error}");
}

#[tokio::test]
async fn timeout_terminates_descendants_before_they_can_escape() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("escaped");
    let program = executable(
        &directory,
        "spawn-descendant",
        &format!(
            "#!/bin/sh\n(sleep 2; touch '{}') &\nsleep 10\n",
            marker.display()
        ),
    );
    let mut command_config = config(&directory, "spawn", program);
    command_config.timeout_seconds = 1;
    let tool = CommandTool::new(command_config).unwrap();

    let error = tool.execute(json!({"program": "spawn"})).await.unwrap_err();
    assert!(error.to_string().contains("1-second timeout"), "{error}");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    assert!(!marker.exists(), "a descendant escaped the command timeout");
}

#[tokio::test]
async fn executes_the_authorized_file_even_if_its_path_is_replaced() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(&directory, "inspect", "#!/bin/sh\nprintf 'authorized'\n");
    let tool = CommandTool::new(config(&directory, "inspect", program.clone())).unwrap();
    std::fs::rename(&program, directory.path().join("authorized-original")).unwrap();
    executable(&directory, "inspect", "#!/bin/sh\nprintf 'replacement'\n");

    let result = tool.execute(json!({"program": "inspect"})).await.unwrap();

    assert_eq!(result["stdout"], "authorized");
}

#[tokio::test]
async fn uses_the_authorized_working_directory_even_if_its_path_is_replaced() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let parent = tempfile::tempdir().unwrap();
    let working_directory = parent.path().join("working");
    std::fs::create_dir(&working_directory).unwrap();
    std::fs::write(working_directory.join("identity"), "authorized").unwrap();
    let program = executable(&parent, "read-identity", "#!/bin/sh\n/bin/cat identity\n");
    let tool = CommandTool::new(CommandConfig {
        working_directory: working_directory.clone(),
        programs: BTreeMap::from([("inspect".to_owned(), program)]),
        timeout_seconds: 5,
        max_output_bytes: 8192,
    })
    .unwrap();
    std::fs::rename(
        &working_directory,
        parent.path().join("authorized-original"),
    )
    .unwrap();
    std::fs::create_dir(&working_directory).unwrap();
    std::fs::write(working_directory.join("identity"), "replacement").unwrap();

    let result = tool.execute(json!({"program": "inspect"})).await.unwrap();

    assert_eq!(result["stdout"], "authorized");
}

#[tokio::test]
async fn terminates_commands_when_combined_output_exceeds_the_limit() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(
        &directory,
        "noisy",
        "#!/bin/sh\nprintf '12345'\nprintf '67890' >&2\n",
    );
    let mut command_config = config(&directory, "noisy", program);
    command_config.max_output_bytes = 9;
    let tool = CommandTool::new(command_config).unwrap();

    let error = tool.execute(json!({"program": "noisy"})).await.unwrap_err();

    assert!(error.to_string().contains("9-byte limit"));
    assert!(
        !error.to_string().contains("cleanup failed"),
        "overflow cleanup failed: {error}"
    );
}

#[tokio::test]
async fn overflow_on_one_stream_is_not_starved_by_an_idle_stream() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(
        &directory,
        "stderr-only",
        "#!/bin/sh\nwhile :; do printf 'overflow' >&2; done\n",
    );
    let mut command_config = config(&directory, "noisy", program);
    command_config.timeout_seconds = 5;
    command_config.max_output_bytes = 64;
    let tool = CommandTool::new(command_config).unwrap();

    let error = tool.execute(json!({"program": "noisy"})).await.unwrap_err();

    assert!(error.to_string().contains("64-byte limit"), "{error}");
}

#[tokio::test]
async fn output_overflow_terminates_descendants_before_they_can_escape() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("escaped-after-output");
    let program = executable(
        &directory,
        "noisy-descendant",
        &format!(
            "#!/bin/sh\n(sleep 2; touch '{}') &\nwhile :; do printf 'overflow'; done\n",
            marker.display()
        ),
    );
    let mut command_config = config(&directory, "noisy", program);
    command_config.max_output_bytes = 64;
    let tool = CommandTool::new(command_config).unwrap();

    let error = tool.execute(json!({"program": "noisy"})).await.unwrap_err();
    assert!(error.to_string().contains("64-byte limit"));
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    assert!(!marker.exists(), "a descendant escaped output cancellation");
}

#[tokio::test]
async fn dropping_execution_terminates_descendants() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("escaped-after-drop");
    let started = directory.path().join("descendant-started");
    let program = executable(
        &directory,
        "cancel-group",
        "#!/bin/sh\ntrap '' HUP TERM\n(\n  trap '' HUP TERM\n  : > \"$2\"\n  sleep 2\n  : > \"$1\"\n) &\nsleep 10\n",
    );
    let tool = Arc::new(CommandTool::new(config(&directory, "cancel", program)).unwrap());
    let execution = {
        let tool = Arc::clone(&tool);
        let arguments = [marker.display().to_string(), started.display().to_string()];
        tokio::spawn(async move {
            tool.execute(json!({"program": "cancel", "arguments": arguments}))
                .await
        })
    };
    for _ in 0..1_000 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(started.exists(), "the descendant did not start in time");
    execution.abort();
    let _ = execution.await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    assert!(!marker.exists(), "a descendant escaped future cancellation");
}

#[tokio::test]
async fn accepts_exact_combined_output_boundary_from_both_streams() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(
        &directory,
        "exact-output",
        "#!/bin/sh\nprintf '12345'\nprintf '6789' >&2\n",
    );
    let mut command_config = config(&directory, "exact", program);
    command_config.max_output_bytes = 9;
    let tool = CommandTool::new(command_config).unwrap();

    let result = tool.execute(json!({"program": "exact"})).await.unwrap();

    assert_eq!(result["stdout"], "12345");
    assert_eq!(result["stderr"], "6789");
}

#[tokio::test]
async fn provides_null_stdin() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(
        &directory,
        "stdin",
        "#!/bin/sh\nif IFS= read -r line; then printf 'data'; else printf 'eof'; fi\n",
    );
    let tool = CommandTool::new(config(&directory, "stdin", program)).unwrap();

    let result = tool.execute(json!({"program": "stdin"})).await.unwrap();

    assert_eq!(result["stdout"], "eof");
}

#[tokio::test]
async fn enforces_argument_count_and_byte_limits() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(&directory, "arguments", "#!/bin/sh\nexit 0\n");
    let tool = CommandTool::new(config(&directory, "arguments", program)).unwrap();

    let count_error = tool
        .execute(json!({"program": "arguments", "arguments": vec!["x"; 129]}))
        .await
        .unwrap_err();
    let byte_error = tool
        .execute(json!({"program": "arguments", "arguments": ["x".repeat(32 * 1024 + 1)]}))
        .await
        .unwrap_err();

    assert!(count_error.to_string().contains("at most 128"));
    assert!(byte_error.to_string().contains("32768-byte limit"));
}

#[tokio::test]
async fn rejects_invalid_utf8_output() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(&directory, "binary", "#!/bin/sh\nprintf '\\377'\n");
    let tool = CommandTool::new(config(&directory, "binary", program)).unwrap();

    let error = tool
        .execute(json!({"program": "binary"}))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("not valid UTF-8"), "{error}");
}

#[tokio::test]
async fn reports_null_exit_code_for_signaled_processes() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let program = executable(&directory, "signaled", "#!/bin/sh\nkill -TERM $$\n");
    let tool = CommandTool::new(config(&directory, "signaled", program)).unwrap();

    let result = tool.execute(json!({"program": "signaled"})).await.unwrap();

    assert_eq!(result["success"], false);
    assert_eq!(result["exit_code"], serde_json::Value::Null);
}

#[test]
fn rejects_invalid_working_directories_and_program_objects() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let working_file = directory.path().join("not-a-directory");
    std::fs::write(&working_file, "content").unwrap();
    let executable_program = executable(&directory, "executable", "#!/bin/sh\nexit 0\n");
    let working_error = CommandTool::new(CommandConfig {
        working_directory: working_file,
        programs: BTreeMap::from([("program".to_owned(), executable_program)]),
        timeout_seconds: 5,
        max_output_bytes: 8192,
    })
    .err()
    .unwrap();

    let program_error = CommandTool::new(CommandConfig {
        working_directory: directory.path().to_owned(),
        programs: BTreeMap::from([("program".to_owned(), directory.path().to_owned())]),
        timeout_seconds: 5,
        max_output_bytes: 8192,
    })
    .err()
    .unwrap();

    assert!(working_error.to_string().contains("not a directory"));
    assert!(program_error.to_string().contains("not a regular file"));
}

#[test]
fn rejects_a_fifo_working_directory_without_blocking() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let working_directory = fifo(&directory, "working-directory.fifo");
    let program = executable(&directory, "program", "#!/bin/sh\nexit 0\n");
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let mut config = config(&directory, "program", program);
        config.working_directory = working_directory;
        let _ = sender.send(CommandTool::new(config).map(|_| ()));
    });

    let result = receiver
        .recv_timeout(std::time::Duration::from_millis(250))
        .expect("opening a FIFO working directory blocked");
    assert!(result.unwrap_err().to_string().contains("not a directory"));
}

#[test]
fn rejects_a_fifo_program_without_blocking() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let program = fifo(&directory, "program.fifo");
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let _ = sender.send(CommandTool::new(config(&directory, "program", program)).map(|_| ()));
    });

    let result = receiver
        .recv_timeout(std::time::Duration::from_millis(250))
        .expect("opening a FIFO program blocked");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not a regular file")
    );
}

#[test]
fn rejects_non_executable_programs() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("not-executable");
    std::fs::write(&program, "content").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();

    let error = CommandTool::new(config(&directory, "program", program))
        .err()
        .unwrap();

    assert!(error.to_string().contains("not executable"));
}

#[tokio::test]
async fn does_not_inherit_the_agent_process_home() {
    let _fixture_guard = FIXTURE_PROCESS_LOCK.lock().await;
    assert!(std::env::var_os("HOME").is_some());
    let directory = tempfile::tempdir().unwrap();
    let program = executable(
        &directory,
        "environment",
        "#!/bin/sh\nprintf '%s' \"${HOME-unset}\"\n",
    );
    let tool = CommandTool::new(config(&directory, "environment", program)).unwrap();

    let result = tool
        .execute(json!({"program": "environment"}))
        .await
        .unwrap();

    assert_eq!(result["stdout"], "unset");
}
