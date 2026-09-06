use rynna_config::ProfileCatalog;
use rynna_core::workflows::default_workflow;
#[test]
fn catalog_workflows_round_trip_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.toml");
    let source = std::fs::read_to_string("../../rynna.example.toml").unwrap();
    std::fs::write(
        &path,
        source
            .split("# Optional profile-owned workflow")
            .next()
            .unwrap(),
    )
    .unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let profile = catalog.default_profile().to_owned();
    assert_eq!(catalog.workflows(&profile).unwrap().len(), 1);
    let mut workflow = default_workflow();
    workflow.id = "custom".into();
    let saved = catalog.save_workflow(&profile, workflow).unwrap();
    assert_eq!(saved.revision, 1);
    let saved = catalog.save_workflow(&profile, saved).unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(
        ProfileCatalog::load(&path)
            .unwrap()
            .workflows(&profile)
            .unwrap()[1],
        saved
    );
    let before = std::fs::read(&path).unwrap();
    let mut bad = saved.clone();
    bad.steps[0].instructions.clear();
    assert!(catalog.save_workflow(&profile, bad).is_err());
    assert_eq!(before, std::fs::read(&path).unwrap());
    assert_eq!(catalog.workflows(&profile).unwrap()[1], saved);
    assert!(catalog.delete_workflow(&profile, "rynna-default").is_err());
    let mut second = catalog.resolve(&profile).unwrap().profile;
    second.name = "isolated".into();
    catalog.add_profile(second).unwrap();
    let mut copy = saved.clone();
    copy.revision = 1;
    catalog.save_workflow("isolated", copy).unwrap();
    catalog.delete_workflow(&profile, "custom").unwrap();
    assert_eq!(catalog.workflows(&profile).unwrap().len(), 1);
    assert_eq!(catalog.workflows("isolated").unwrap()[1].id, "custom");
    let before = catalog.workflows("isolated").unwrap();
    std::fs::rename(&path, dir.path().join("backup.toml")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        catalog
            .save_workflow("isolated", before[1].clone())
            .is_err()
    );
    assert_eq!(catalog.workflows("isolated").unwrap(), before);
}

#[test]
fn example_workflow_configuration_is_valid() {
    let source = std::fs::read_to_string("../../rynna.example.toml").unwrap();
    let catalog = ProfileCatalog::from_toml(&source).unwrap();
    assert_eq!(catalog.workflows("local").unwrap().len(), 2);
}
