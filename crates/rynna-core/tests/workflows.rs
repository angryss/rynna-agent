use rynna_core::workflows::*;
#[test]
fn default_and_validation_boundaries() {
    let original = default_workflow();
    original.validate(&[]).unwrap();
    let mut custom = original.clone();
    custom.id = "custom".into();
    validate_custom(&[custom.clone()], &[]).unwrap();
    assert!(validate_custom(std::slice::from_ref(&original), &[]).is_err());
    assert!(validate_custom(&vec![custom.clone(); 33], &[]).is_err());
    assert!(validate_custom(&[custom.clone(), custom.clone()], &[]).is_err());
    for target in ["verify", "missing"] {
        let mut w = custom.clone();
        w.steps[2].repeat_target = Some(target.into());
        assert!(w.validate(&[]).is_err());
    }
    let mut w = custom.clone();
    w.steps[1].id = "plan".into();
    assert!(w.validate(&[]).is_err());
    let mut w = custom.clone();
    w.steps[0].instructions = " ".into();
    assert!(w.validate(&[]).is_err());
    w.steps[0].instructions = "x".repeat(32001);
    assert!(w.validate(&[]).is_err());
    let mut w = custom.clone();
    w.steps[0].executor = Executor::Subagent;
    w.steps[0].helper = Some("review".into());
    assert!(w.validate(&[]).is_err());
    let h = rynna_core::Subagent {
        name: "review".into(),
        description: "Review".into(),
        instructions: "Review work".into(),
    };
    w.validate(&[h]).unwrap();
    let metadata = serde_json::to_string(&original.metadata()).unwrap();
    assert!(!metadata.contains("instructions"));
    assert!(!metadata.contains("steps"));
    custom.steps[0].instructions = "customized".into();
    assert_eq!(original, default_workflow());
}
