#![cfg(target_os = "macos")]

use rynna_core::Tool;
use rynna_tools_command::{CommandConfig, CommandTool};
use serde_json::json;
use std::collections::BTreeMap;

#[tokio::test]
async fn system_commands_can_inspect_macos() {
    let tool = CommandTool::new(CommandConfig {
        working_directory: "/".into(),
        programs: BTreeMap::from([
            ("uname".to_owned(), "/usr/bin/uname".into()),
            ("sw_vers".to_owned(), "/usr/bin/sw_vers".into()),
        ]),
        timeout_seconds: 5,
        max_output_bytes: 65536,
    })
    .unwrap();
    for (program, expected) in [("uname", "Darwin"), ("sw_vers", "macOS")] {
        let result = tool.execute(json!({"program": program})).await.unwrap();
        assert_eq!(result["success"], true, "{program}: {result}");
        assert!(
            result["stdout"].as_str().unwrap().contains(expected),
            "{result}"
        );
    }
}

#[tokio::test]
async fn replacing_a_symlink_cannot_change_the_authorized_system_program() {
    let directory = tempfile::tempdir().unwrap();
    let alias = directory.path().join("inspect");
    std::os::unix::fs::symlink("/usr/bin/uname", &alias).unwrap();
    let tool = CommandTool::new(CommandConfig {
        working_directory: directory.path().to_owned(),
        programs: BTreeMap::from([("inspect".to_owned(), alias.clone())]),
        timeout_seconds: 5,
        max_output_bytes: 65536,
    })
    .unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("/bin/echo", &alias).unwrap();
    let result = tool.execute(json!({"program": "inspect"})).await.unwrap();
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["stdout"], "Darwin\n");
}
