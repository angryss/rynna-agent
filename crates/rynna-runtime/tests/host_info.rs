use rynna_config::ProfileCatalog;
use serde_json::json;

#[tokio::test]
async fn host_info_reports_live_metadata_and_accepts_only_empty_object() {
    let profile = ProfileCatalog::built_in().resolve("default").unwrap();
    let tools = rynna_runtime::native_tools(&profile).unwrap();
    let host = tools
        .iter()
        .find(|t| t.definition().name == "host_info")
        .unwrap();
    for invalid in [
        json!(null),
        json!([]),
        json!(""),
        json!({"path":"/etc/passwd"}),
    ] {
        assert!(host.execute(invalid).await.is_err());
    }
    let value = host.execute(json!({})).await.unwrap();
    assert_eq!(value["os"], std::env::consts::OS);
    assert_eq!(value["architecture"], std::env::consts::ARCH);
    assert_eq!(
        value["working_directory"],
        json!(std::env::current_dir().unwrap())
    );
    #[cfg(target_os = "linux")]
    if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
        // The live smoke test independently checks ordinary quoted/unquoted NAME.
        if let Some(name) = release.lines().find_map(|line| line.strip_prefix("NAME=")) {
            assert_eq!(
                value["distribution"]["name"],
                name.trim_matches('"').trim_matches('\'')
            );
        }
    }
    println!("host_info: {value}");
}
