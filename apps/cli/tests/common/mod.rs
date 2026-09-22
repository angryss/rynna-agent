pub fn has_history(
    request: &wiremock::Request,
    history: serde_json::Value,
    configured: &str,
) -> bool {
    let body: serde_json::Value = request.body_json().unwrap();
    let messages = body["messages"].as_array().unwrap();
    let system = messages[0]["content"].as_str().unwrap();
    let inventory = system
        .split("Available tools for this request (JSON):\n")
        .nth(1)
        .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok());
    let names = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    messages[0]["role"] == "system"
        && system.contains("Reason -> Act -> Observe")
        && system.contains(configured)
        && inventory == Some(names)
        && serde_json::Value::from(messages[1..].to_vec()) == history
}
