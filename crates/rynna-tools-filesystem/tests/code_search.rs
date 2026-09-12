use rynna_core::Tool;
use rynna_tools_filesystem::{FileSystemConfig, FileSystemToolset};
use serde_json::json;
use std::{sync::Arc, time::Instant};

fn search_tool(root: &std::path::Path, cache: &std::path::Path) -> Arc<dyn Tool> {
    let mut config = FileSystemConfig::new(root);
    config.code_search_cache = Some(cache.to_owned());
    FileSystemToolset::new(config)
        .unwrap()
        .tools()
        .into_iter()
        .find(|tool| tool.definition().name == "code_search")
        .unwrap()
}

#[tokio::test]
async fn persists_index_and_applies_content_changes_renames_deletions_and_ignores() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/a.rs"), "fn needle_one() {}\n").unwrap();
    std::fs::write(root.path().join("src/b.rs"), "fn needle_two() {}\n").unwrap();
    let tool = search_tool(root.path(), cache.path());
    let cold = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(cold["matches"].as_array().unwrap().len(), 2);
    assert_eq!(cold["index"]["files_updated"], 2);
    drop(tool);
    let tool = search_tool(root.path(), cache.path());
    let warm = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(warm["index"]["files_read"], 0);
    assert_eq!(warm["index"]["files_updated"], 0);
    std::fs::write(root.path().join("src/a.rs"), "fn needle_new() {}\n").unwrap();
    std::fs::rename(root.path().join("src/b.rs"), root.path().join("src/c.rs")).unwrap();
    let changed = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(changed["index"]["files_updated"], 2);
    assert_eq!(changed["index"]["files_removed"], 1);
    assert!(
        changed["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["text"] == "fn needle_new() {}")
    );
    std::fs::remove_file(root.path().join("src/a.rs")).unwrap();
    std::fs::write(root.path().join(".gitignore"), "src/c.rs\n").unwrap();
    let removed = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(removed["matches"], json!([]));
    assert_eq!(removed["index"]["files_removed"], 2);
    std::fs::write(root.path().join(".gitignore"), "").unwrap();
    let restored = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(restored["matches"][0]["path"], "src/c.rs");
}

#[tokio::test]
async fn respects_policy_nested_ignores_scopes_and_unicode_literals() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for directory in ["src", "other", ".git", "generated"] {
        std::fs::create_dir(root.path().join(directory)).unwrap();
    }
    for file in [
        "src/keep.rs",
        "src/omit.rs",
        "other/keep.rs",
        ".git/config",
        ".env",
        "generated/omit.rs",
    ] {
        std::fs::write(root.path().join(file), "needle Foo::bar() 日本語の検索\n").unwrap();
    }
    std::fs::write(root.path().join(".gitignore"), "generated/\n").unwrap();
    std::fs::write(root.path().join("src/.ignore"), "*.rs\n!keep.rs\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join(".env"), root.path().join("leak.rs")).unwrap();
    let tool = search_tool(root.path(), cache.path());
    let result = tool
        .execute(json!({"query":"needle", "include_glob":"**/*.rs"}))
        .await
        .unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    for query in ["Foo::bar()", "日本語", "needle"] {
        let result = tool
            .execute(json!({"query":query,"path":"src"}))
            .await
            .unwrap();
        assert_eq!(
            result["matches"].as_array().unwrap().len(),
            1,
            "{query}: {result}"
        );
        assert_eq!(result["matches"][0]["path"], "src/keep.rs");
    }
    assert!(tool.execute(json!({"query":"ne"})).await.is_err());
    assert!(
        tool.execute(json!({"query":"needle","path":"../"}))
            .await
            .is_err()
    );
    let result = tool.execute(json!({"query":"NEEDLE"})).await.unwrap();
    assert_eq!(result["matches"], json!([]));
}

#[tokio::test]
async fn bounds_output_and_reports_oversized_binary_and_stale_replacements() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("long.rs"),
        format!("{}needle{}", "x".repeat(10000), "y".repeat(10000)),
    )
    .unwrap();
    std::fs::write(root.path().join("large.rs"), "z".repeat(1024 * 1024 + 1)).unwrap();
    std::fs::write(root.path().join("binary"), b"needle\0secret").unwrap();
    let tool = search_tool(root.path(), cache.path());
    let result = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    assert_eq!(result["matches"][0]["snippet_truncated"], true);
    assert_eq!(result["index"]["files_skipped"], 2);
    assert!(result.to_string().len() < 13000);
    std::fs::write(root.path().join("long.rs"), b"needle\0binary").unwrap();
    let result = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(result["matches"], json!([]));
    std::fs::write(root.path().join("long.rs"), "needle\n".repeat(1000)).unwrap();
    let result = tool
        .execute(json!({"query":"needle", "max_results":2}))
        .await
        .unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    assert_eq!(result["truncated"], true);
}

#[tokio::test]
async fn repository_and_policy_indexes_are_isolated() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("a.rs"), "needle").unwrap();
    std::fs::write(b.path().join("b.rs"), "needle").unwrap();
    let first = search_tool(a.path(), cache.path())
        .execute(json!({"query":"needle"}))
        .await
        .unwrap();
    let second = search_tool(b.path(), cache.path())
        .execute(json!({"query":"needle"}))
        .await
        .unwrap();
    assert_eq!(first["matches"][0]["path"], "a.rs");
    assert_eq!(second["matches"][0]["path"], "b.rs");
    let mut config = FileSystemConfig::new(a.path());
    config.code_search_cache = Some(cache.path().to_owned());
    config.denied_patterns.push("a.rs".into());
    let restricted = FileSystemToolset::new(config)
        .unwrap()
        .tools()
        .into_iter()
        .find(|t| t.definition().name == "code_search")
        .unwrap();
    assert_eq!(
        restricted.execute(json!({"query":"needle"})).await.unwrap()["matches"],
        json!([])
    );
}

/// Run explicitly: cargo test -p rynna-tools-filesystem --test code_search large_repository -- --ignored --nocapture
#[tokio::test]
#[ignore = "generates a 100,000-file / 200+ MB synthetic monorepo"]
async fn large_repository() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let count: usize = std::env::var("RYNNA_SEARCH_BENCH_FILES")
        .ok()
        .map(|value| value.parse().unwrap())
        .unwrap_or(100_000);
    assert!(count >= 1000 && count.is_multiple_of(1000));
    let directories = count / 1000;
    let target_path = format!("pkg{}/file999.rs", directories - 1);
    let query = format!("symbol_{}_999", directories - 1);
    let padding = "// representative source padding\n".repeat(64);
    for directory in 0..directories {
        std::fs::create_dir(root.path().join(format!("pkg{directory}"))).unwrap();
        for index in 0..1000 {
            std::fs::write(
                root.path().join(format!("pkg{directory}/file{index}.rs")),
                format!("fn symbol_{directory}_{index}() {{}}\n{padding}"),
            )
            .unwrap();
        }
    }
    let tool = search_tool(root.path(), cache.path());
    let started = Instant::now();
    let cold = finish(&tool, json!({"query":query})).await;
    let cold_time = started.elapsed();
    assert_eq!(cold["index"]["files_updated"], count);
    assert_eq!(cold["matches"][0]["path"], target_path.as_str());
    assert_eq!(cold["candidates_examined"], 1);
    drop(tool);
    let tool = search_tool(root.path(), cache.path());
    let started = Instant::now();
    let warm = finish(&tool, json!({"query":query})).await;
    let warm_time = started.elapsed();
    assert_eq!(warm["index"]["files_read"], 0);
    assert_eq!(warm["index"]["files_updated"], 0);
    std::fs::write(
        root.path().join(target_path.as_str()),
        "fn changed_target() {}\n",
    )
    .unwrap();
    let started = Instant::now();
    let changed = finish(&tool, json!({"query":"changed_target"})).await;
    let changed_time = started.elapsed();
    assert_eq!(changed["index"]["files_read"], 1);
    assert_eq!(changed["index"]["files_updated"], 1);
    assert_eq!(changed["matches"][0]["path"], target_path.as_str());
    println!(
        "files={count}; cold={cold_time:?}; warm={warm_time:?}; one_edit={changed_time:?}; bytes_read={}; result_bytes={}",
        cold["index"]["bytes_read"],
        changed.to_string().len()
    );
    let databases: Vec<_> = std::fs::read_dir(cache.path())
        .unwrap()
        .map(|e| e.unwrap().path().join("index.sqlite3"))
        .collect();
    println!(
        "index_bytes={}",
        std::fs::metadata(&databases[0]).unwrap().len()
    );
}

async fn finish(tool: &Arc<dyn Tool>, arguments: serde_json::Value) -> serde_json::Value {
    loop {
        let result = tool.execute(arguments.clone()).await.unwrap();
        if result["status"] != "indexing" {
            return result;
        }
    }
}

#[tokio::test]
async fn index_handles_concurrent_instances_and_excludes_its_own_cache() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    std::fs::write(root.path().join("main.rs"), "needle").unwrap();
    let first = search_tool(root.path(), &cache);
    let second = search_tool(root.path(), &cache);
    let (a, b) = tokio::join!(
        first.execute(json!({"query":"needle"})),
        second.execute(json!({"query":"needle"}))
    );
    for result in [a.unwrap(), b.unwrap()] {
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["index"]["files_checked"], 1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let index = std::fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::metadata(index).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_file(root.path().join("main.rs")).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "needle outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("main.rs")).unwrap();
        let result = first.execute(json!({"query":"needle"})).await.unwrap();
        assert_eq!(result["matches"], json!([]));
        assert_eq!(result["index"]["files_removed"], 1);
    }
}

#[tokio::test]
async fn timestamp_only_updates_do_not_rebuild_and_query_syntax_is_literal() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let content = "needle \"quoted\" value a+b[c]d foo_bar\n";
    std::fs::write(root.path().join("main.rs"), content).unwrap();
    std::fs::write(root.path().join("binary"), b"needle\0").unwrap();
    let tool = search_tool(root.path(), cache.path());
    tool.execute(json!({"query":"needle"})).await.unwrap();
    let warm = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(warm["index"]["files_read"], 0);
    assert_eq!(warm["index"]["files_skipped"], 1);
    std::fs::write(root.path().join("main.rs"), content).unwrap();
    let touched = tool.execute(json!({"query":"needle"})).await.unwrap();
    assert_eq!(touched["index"]["files_updated"], 0);
    for query in ["\"quoted\"", "a+b[c]d", "foo_bar"] {
        let result = tool.execute(json!({"query":query})).await.unwrap();
        assert_eq!(result["matches"][0]["path"], "main.rs", "{query}: {result}");
    }
}
