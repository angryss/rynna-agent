#![cfg(unix)]
use rynna_core::process::ProcessGroup;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

#[tokio::test]
async fn dropping_provider_process_group_kills_descendants_even_after_parent_exit() {
    for parent_exits in [false, true] {
        let directory =
            std::env::temp_dir().join(format!("rynna-process-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("escaped");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(if parent_exits {
                "(sleep 0.5; touch \"$1\") & echo ready"
            } else {
                "(sleep 0.5; touch \"$1\") & echo ready; wait"
            })
            .arg("test")
            .arg(&marker)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (mut child, group) = ProcessGroup::spawn(&mut command).unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(line.trim(), "ready");
        if parent_exits {
            child.wait().await.unwrap();
        }
        drop(group);
        child.wait().await.unwrap();
        tokio::time::sleep(Duration::from_millis(750)).await;
        assert!(!marker.exists(), "provider descendant escaped cancellation");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
