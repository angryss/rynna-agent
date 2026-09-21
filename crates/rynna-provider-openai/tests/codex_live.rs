//! Opt-in authenticated smoke test. Never run as part of the default suite.
//! RYNNA_LIVE_CODEX_MODEL=<model> cargo test -p rynna-provider-openai \
//!   --features tokio/fs --test codex_live -- --ignored --nocapture
use rynna_core::{CompletionRequest, Message, ModelProvider, ThinkingLevel, ToolDefinition};
use rynna_provider_openai::CodexAppServerProvider;

#[tokio::test]
#[ignore = "uses the selected Rynna account and makes real inference requests"]
async fn account_model_respects_no_tools_and_round_trips_an_explicit_tool() {
    let model =
        std::env::var("RYNNA_LIVE_CODEX_MODEL").expect("set RYNNA_LIVE_CODEX_MODEL explicitly");
    let profile = std::env::var("RYNNA_LIVE_CODEX_PROFILE").unwrap_or_else(|_| "default".into());
    let provider = CodexAppServerProvider::for_profile(
        rynna_config::ProviderSettingsStore::default_path().unwrap(),
        &profile,
        &model,
    )
    .unwrap()
    .with_thinking(ThinkingLevel::Medium)
    .unwrap();
    let prompt = "What operating system is installed on this computer?";
    let answer = provider
        .complete(CompletionRequest {
            messages: vec![Message::user(prompt)],
            tools: vec![],
        })
        .await
        .unwrap();
    assert!(answer.message.tool_calls.is_empty());
    assert!(!answer.message.content.is_empty());
    println!("No-tools request completed without invoking ambient tools.");

    let request = CompletionRequest {
        messages: vec![Message::user(format!(
            "{prompt} Use the provided inspect_host tool before answering."
        ))],
        tools: vec![ToolDefinition::new(
            "inspect_host",
            "Read this host's /etc/os-release",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        )],
    };
    let call = provider.complete(request.clone()).await.unwrap();
    assert_eq!(call.message.tool_calls.len(), 1);
    assert_eq!(call.message.tool_calls[0].name, "inspect_host");
    let os = std::fs::read_to_string("/etc/os-release").unwrap();
    let result = Message::tool(&call.message.tool_calls[0].id, os);
    let mut next = request;
    next.messages.push(call.message);
    next.messages.push(result);
    let answer = provider.complete(next).await.unwrap();
    assert!(answer.message.tool_calls.is_empty());
    assert!(!answer.message.content.is_empty());
    println!(
        "Explicit host tool call and native result replay completed: {}",
        answer.message.content
    );
}
