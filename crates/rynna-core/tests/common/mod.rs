use rynna_core::{CompletionRequest, Role};

pub fn assert_policy(request: &CompletionRequest) {
    let system = &request.messages[0];
    assert_eq!(system.role, Role::System);
    for instruction in [
        "software development",
        "terminal",
        "Reason -> Act -> Observe",
        "privately",
        "Do not expose",
        "Available tools for this request",
    ] {
        assert!(
            system.content.contains(instruction),
            "missing {instruction}"
        );
    }
    assert_eq!(
        system.content.matches("Reason -> Act -> Observe").count(),
        1
    );
    let inventory = system
        .content
        .split("Available tools for this request (JSON):\n")
        .nth(1)
        .unwrap();
    let names: Vec<String> = serde_json::from_str(inventory).unwrap();
    assert_eq!(
        names,
        request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    );
}
