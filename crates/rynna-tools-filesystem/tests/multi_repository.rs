use async_trait::async_trait;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError, Tool, ToolCall,
};
use rynna_tools_filesystem::{FileSystemConfig, FileSystemToolset};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn repository(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    std::fs::create_dir(path.join("src")).unwrap();
    std::fs::write(
        path.join("src/main.rs"),
        format!("fn needle_{name}() {{}}\n"),
    )
    .unwrap();
    path
}
fn tool(root: &Path, cache: &Path) -> Arc<dyn Tool> {
    let mut config = FileSystemConfig::new(root);
    config.code_search_cache = Some(cache.to_owned());
    FileSystemToolset::new(config)
        .unwrap()
        .tools()
        .into_iter()
        .find(|tool| tool.definition().name == "code_search")
        .unwrap()
}
fn databases(cache: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(cache)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("index.sqlite3"))
        .collect()
}
fn stored_paths(database: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(database).unwrap();
    db.prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
struct Model;
#[async_trait]
impl ModelProvider for Model {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        if let Some(message) = request
            .messages
            .iter()
            .find(|message| message.tool_call_id.is_some())
        {
            Ok(Completion::new(Message::assistant(message.content.clone())))
        } else {
            Ok(Completion::with_tool_calls(vec![ToolCall::new(
                "search",
                "code_search",
                json!({"query":"needle"}),
            )]))
        }
    }
}

#[tokio::test]
async fn sessions_search_unique_reusable_repository_indexes_without_mutating_other_sessions() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = repository(root.path(), "backend");
    let b = repository(root.path(), "other/backend");
    repository(root.path(), "unrelated");
    // Worktree .git files are repository boundaries, never read as source or followed.
    std::fs::remove_dir(b.join(".git")).unwrap();
    std::fs::write(b.join(".git"), "gitdir: /unavailable/admin-directory").unwrap();
    let profile: Profile = serde_json::from_value(json!({"name":"test","providers":[],
    "projects":[
        {"name":"both","directories":[a,b],"default_directory":a},
        {"name":"a","directories":[a],"default_directory":a}
    ]}))
    .unwrap();
    let profiles = AgentProfiles::new(
        "test",
        [(
            profile,
            Agent::with_tools(
                Arc::new(Model),
                "policy",
                vec![tool(root.path(), cache.path())],
            )
            .unwrap(),
        )],
    )
    .unwrap();
    let both = profiles.clone().with_project(None, Some("both")).unwrap();
    let only_a = profiles.clone().with_project(None, Some("a")).unwrap();
    let result: Value =
        serde_json::from_str(&both.respond(None, &[], "search").await.unwrap().content).unwrap();
    assert_eq!(result["status"], "ready");
    assert_eq!(result["repository_count"], 2);
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    let identities: std::collections::BTreeSet<_> = result["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["repository"].as_str().unwrap())
        .collect();
    assert_eq!(identities.len(), 2);
    assert!(
        result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["repository_path"] == "src/main.rs")
    );
    assert_eq!(databases(cache.path()).len(), 2);
    let mut paths: Vec<_> = databases(cache.path())
        .iter()
        .map(|db| stored_paths(db))
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            vec!["backend/src/main.rs"],
            vec!["other/backend/src/main.rs"]
        ]
    );

    std::fs::write(b.join("src/main.rs"), "fn needle_changed() {}\n").unwrap();
    let result: Value =
        serde_json::from_str(&only_a.respond(None, &[], "search").await.unwrap().content).unwrap();
    assert_eq!(result["repository_count"], 1);
    assert_eq!(result["index"]["files_read"], 0);
    assert_eq!(result["matches"][0]["path"], "backend/src/main.rs");
    // Fresh request snapshots reuse the same indexes; only B changed.
    let result: Value = serde_json::from_str(
        &profiles
            .clone()
            .with_project(None, Some("both"))
            .unwrap()
            .respond(None, &[], "search")
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(result["index"]["files_read"], 1);
    assert_eq!(result["index"]["files_updated"], 1);
    assert_eq!(databases(cache.path()).len(), 2);
    let a_stats = result["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["repository"] == a.canonicalize().unwrap().to_string_lossy().as_ref())
        .unwrap();
    assert_eq!(a_stats["index"]["files_read"], 0);
}

#[tokio::test]
async fn nested_repositories_and_overlapping_directories_do_not_duplicate_files() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let parent = repository(root.path(), "parent");
    let child = repository(&parent, "child");
    let search = tool(root.path(), cache.path())
        .for_project(&[parent.clone(), parent.join("src"), child.clone()])
        .unwrap();
    let result = search.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(result["repository_count"], 2);
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    assert_eq!(result["index"]["files_read"], 2);
    assert_eq!(databases(cache.path()).len(), 2);
    let mut paths: Vec<_> = databases(cache.path())
        .iter()
        .map(|db| stored_paths(db))
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![vec!["parent/child/src/main.rs"], vec!["parent/src/main.rs"]]
    );
    let scoped = search
        .execute(json!({"query":"needle","path":"parent/child/src"}))
        .await
        .unwrap();
    assert_eq!(scoped["repository_count"], 1);
    assert_eq!(scoped["index"]["files_read"], 0);
    assert_eq!(scoped["matches"][0]["path"], "parent/child/src/main.rs");
    std::fs::write(child.join(".gitignore"), "src/\n").unwrap();
    let result = search.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    assert_eq!(result["index"]["files_removed"], 1);
}

#[tokio::test]
async fn shared_limits_and_repository_failure_keep_other_indexes_usable() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = repository(root.path(), "a");
    let b = repository(root.path(), "b");
    for repo in [&a, &b] {
        std::fs::write(repo.join("src/main.rs"), "needle\n".repeat(200)).unwrap();
    }
    let search = tool(root.path(), cache.path())
        .for_project(&[a, b])
        .unwrap();
    let result = search
        .execute(json!({"query":"needle","max_results":3}))
        .await
        .unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 3);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["repository_count"], 2);
    let result = search
        .execute(json!({"query":"needle","max_results":100}))
        .await
        .unwrap();
    assert!(result.to_string().len() < 13000);
    let bad = databases(cache.path())
        .into_iter()
        .find(|db| stored_paths(db) == ["a/src/main.rs"])
        .unwrap();
    std::fs::write(bad, "not a database").unwrap();
    let result = search.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["failed_repositories"], 1);
    assert_eq!(result["matches"][0]["path"], "b/src/main.rs");
}

#[tokio::test]
async fn session_scoping_cannot_expand_filesystem_access_or_follow_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = repository(root.path(), "a");
    repository(root.path(), "b");
    let outside = tempfile::tempdir().unwrap();
    let unbound = tool(root.path(), cache.path());
    let escaped = unbound
        .for_project(&[a.clone(), outside.path().to_owned()])
        .unwrap();
    assert!(escaped.execute(json!({"query":"needle"})).await.is_err());
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    let selected = unbound.for_project(std::slice::from_ref(&a)).unwrap();
    assert!(
        selected
            .execute(json!({"query":"needle","path":"b"}))
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&a, root.path().join("alias")).unwrap();
        let alias = unbound.for_project(&[root.path().join("alias")]).unwrap();
        assert!(alias.execute(json!({"query":"needle"})).await.is_err());
    }
    std::fs::write(root.path().join(".gitignore"), "a/\n").unwrap();
    let ignored = selected.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(ignored["status"], "partial");
    assert_eq!(ignored["matches"], json!([]));
}

#[tokio::test]
async fn newly_nested_repository_is_moved_out_of_the_parent_index() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let parent = repository(root.path(), "parent");
    let child = repository(&parent, "child");
    std::fs::remove_dir(child.join(".git")).unwrap();
    let search = tool(root.path(), cache.path())
        .for_project(std::slice::from_ref(&parent))
        .unwrap();
    let flat = search.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(flat["repository_count"], 1);
    let parent_db = databases(cache.path()).pop().unwrap();
    assert_eq!(stored_paths(&parent_db).len(), 2);
    std::fs::create_dir(child.join(".git")).unwrap();
    let split = search.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(split["repository_count"], 2);
    assert_eq!(split["matches"].as_array().unwrap().len(), 2);
    assert_eq!(split["index"]["files_removed"], 1);
    assert_eq!(stored_paths(&parent_db), ["parent/src/main.rs"]);
    assert_eq!(databases(cache.path()).len(), 2);
}

#[tokio::test]
async fn non_git_directories_use_separate_fallback_indexes_and_deduplicate_overlaps() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = repository(root.path(), "a");
    let b = repository(root.path(), "b");
    for path in [&a, &b] {
        std::fs::remove_dir(path.join(".git")).unwrap();
    }
    let search = tool(root.path(), cache.path())
        .for_project(&[a.clone(), a.join("src"), b])
        .unwrap();
    let result = search.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(result["repository_count"], 2);
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    assert_eq!(databases(cache.path()).len(), 2);
}

#[tokio::test]
#[ignore = "24,000-file cross-repository regression; run explicitly"]
async fn large_multi_repository() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = repository(root.path(), "a");
    let b = repository(root.path(), "b");
    for repo in [&a, &b] {
        std::fs::remove_file(repo.join("src/main.rs")).unwrap();
        for number in 0..12_000 {
            let content = if number == 11_999 {
                "needle_last".to_owned()
            } else {
                format!("fn source_{number}() {{}}")
            };
            std::fs::write(repo.join(format!("src/file{number}.rs")), content).unwrap();
        }
    }
    let search = tool(root.path(), cache.path())
        .for_project(&[a.clone(), b.clone()])
        .unwrap();
    let start = std::time::Instant::now();
    let cold = completed(&search).await;
    let cold_elapsed = start.elapsed();
    assert_eq!(cold["index"]["files_read"], 24_000);
    assert_eq!(cold["repository_count"], 2);
    assert_eq!(cold["matches"].as_array().unwrap().len(), 2);
    assert_eq!(cold["candidates_examined"], 2);
    assert_eq!(databases(cache.path()).len(), 2);
    drop(search);
    let search = tool(root.path(), cache.path())
        .for_project(&[a, b.clone()])
        .unwrap();
    let start = std::time::Instant::now();
    let warm = completed(&search).await;
    let warm_elapsed = start.elapsed();
    assert_eq!(warm["index"]["files_read"], 0);
    std::fs::write(b.join("src/file11999.rs"), "needle_last_changed").unwrap();
    let changed = completed(&search).await;
    assert_eq!(changed["index"]["files_read"], 1);
    assert_eq!(changed["index"]["files_updated"], 1);
    // A narrow query must reach its match even with >10,000 matching files elsewhere.
    std::fs::create_dir(b.join("src/narrow")).unwrap();
    std::fs::write(b.join("src/narrow/target.rs"), "fn needle_target() {}").unwrap();
    let narrow = search
        .execute(json!({"query":"fn ","path":"b/src/narrow"}))
        .await
        .unwrap();
    assert_eq!(narrow["repository_count"], 1);
    assert_eq!(narrow["candidates_examined"], 1);
    assert_eq!(narrow["matches"][0]["path"], "b/src/narrow/target.rs");
    println!(
        "repositories=2; files=24000; cold={cold_elapsed:?}; reopened={warm_elapsed:?}; changed_source_reads=1"
    );
}
async fn completed(tool: &Arc<dyn Tool>) -> Value {
    loop {
        let value = tool.execute(json!({"query":"needle_last"})).await.unwrap();
        if value["status"] != "indexing" {
            return value;
        }
    }
}
