#[cfg(unix)]
mod unix {
    use rynna_core::{CompletionDelta, CompletionRequest, Message, ModelProvider, ToolDefinition};
    use rynna_provider_anthropic::ClaudeCodeProvider;
    use serde_json::json;
    use std::fs;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[tokio::test]
    async fn subscription_mode_rejects_version_prefix_spoofing() {
        let program = fixture("fake_claude_version_prefix.sh");
        let provider = ClaudeCodeProvider::new(program, "sonnet");

        let error = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: vec![],
            })
            .await
            .expect_err("a version sharing only the reviewed prefix must fail closed");

        assert!(
            error
                .to_string()
                .contains("unsupported Claude Code version"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn subscription_mode_rejects_version_suffix_spoofing() {
        let provider = ClaudeCodeProvider::new(
            fixture("fake_claude_version_suffix.sh"),
            "claude-sonnet-4-6",
        );

        let error = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: vec![],
            })
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported Claude Code version"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn subscription_mode_rejects_a_failed_version_probe() {
        let program = fixture("fake_claude_version_failure.sh");
        let provider = ClaudeCodeProvider::new(program, "sonnet");

        let error = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: vec![],
            })
            .await
            .expect_err("a failed version probe must fail closed");

        assert!(
            error
                .to_string()
                .contains("version check did not complete successfully")
        );
    }

    #[tokio::test]
    async fn subscription_mode_uses_headless_cli_with_all_tools_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let program = fixture("fake_claude.sh");
        let args = dir.path().join("args");
        let stdin = dir.path().join("stdin");
        let provider = ClaudeCodeProvider::new(&program, "sonnet")
            .with_test_environment("RYNNA_TEST_ARGS", &args)
            .with_test_environment("RYNNA_TEST_STDIN", &stdin)
            .with_test_environment("RYNNA_TEST_SCENARIO", "headless")
            .with_thinking(rynna_core::ThinkingLevel::High)
            .unwrap();
        let mut deltas = vec![];
        let result = provider
            .complete_stream(
                CompletionRequest {
                    messages: vec![Message::system("Be useful"), Message::user("Hello")],
                    tools: vec![],
                },
                &mut |d| deltas.push(d.clone()),
            )
            .await
            .unwrap();
        let args = fs::read_to_string(args).unwrap();
        assert!(args.contains("--effort\nhigh"));
        assert!(args.contains("--print"));
        assert!(args.contains("--output-format\nstream-json"));
        assert!(
            args.contains("--tools\n"),
            "tools were not disabled: {args}"
        );
        assert!(args.contains("--disable-slash-commands"));
        assert!(args.contains("--no-session-persistence"));
        assert!(args.contains("--safe-mode"));
        assert!(args.contains("--no-chrome"));
        assert!(args.contains("--disallowedTools\nmcp__*"));
        assert_eq!(
            result.message,
            Message::assistant("Hello from subscription")
        );
        assert_eq!(
            deltas,
            vec![CompletionDelta::Content("Hello from subscription".into())]
        );
        let prompt = fs::read_to_string(stdin).unwrap();
        assert!(prompt.contains("Be useful"));
        assert!(prompt.contains("Hello"));
    }

    #[tokio::test]
    async fn subscription_mode_resolves_a_relative_executable_before_isolating_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let relative_program = fixture("fake_claude.sh")
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_owned();
        assert!(relative_program.components().count() > 1);
        let provider = ClaudeCodeProvider::new(relative_program, "sonnet")
            .with_test_environment("RYNNA_TEST_ARGS", dir.path().join("args"))
            .with_test_environment("RYNNA_TEST_STDIN", dir.path().join("stdin"))
            .with_test_environment("RYNNA_TEST_SCENARIO", "headless");

        let completion = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: vec![],
            })
            .await
            .unwrap();

        assert_eq!(
            completion.message,
            Message::assistant("Hello from subscription")
        );
    }

    #[tokio::test]
    async fn subscription_mode_bridges_rynna_tools_without_enabling_ambient_tools() {
        let program = fixture("fake_claude_tools.py");
        let provider = ClaudeCodeProvider::new(program, "sonnet");
        let completion = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hi")],
                tools: vec![ToolDefinition::new(
                    "read_file",
                    "Read",
                    json!({"type":"object"}),
                )],
            })
            .await
            .unwrap();
        assert!(provider.supports_external_tools());
        assert_eq!(
            completion.message.tool_calls,
            vec![rynna_core::ToolCall::new(
                "call-1",
                "read_file",
                json!({"path":"test.txt"})
            )]
        );
        let continued = provider
            .complete(CompletionRequest {
                messages: vec![
                    Message::user("Hi"),
                    completion.message,
                    Message::tool("call-1", "file contents"),
                ],
                tools: vec![ToolDefinition::new(
                    "read_file",
                    "Read",
                    json!({"type":"object"}),
                )],
            })
            .await
            .unwrap();
        assert_eq!(continued.message, Message::assistant("Read: file contents"));
    }

    #[tokio::test]
    async fn subscription_bridge_rejects_native_execution_and_ambiguous_results() {
        for scenario in ["native", "native-stream", "missing", "duplicate"] {
            let mut deltas = vec![];
            let result = ClaudeCodeProvider::new(fixture("fake_claude_tools.py"), "sonnet")
                .with_test_environment("RYNNA_TEST_SCENARIO", scenario)
                .complete_stream(
                    CompletionRequest {
                        messages: vec![Message::user("Hi")],
                        tools: vec![ToolDefinition::new(
                            "read_file",
                            "Read",
                            json!({"type":"object"}),
                        )],
                    },
                    &mut |d| deltas.push(d.clone()),
                )
                .await;
            assert!(result.is_err(), "accepted {scenario}");
            assert!(
                deltas.is_empty(),
                "leaked intermediary output for {scenario}"
            );
        }
    }

    #[tokio::test]
    async fn subscription_bridge_rejects_reused_call_ids_before_dispatch() {
        let result = ClaudeCodeProvider::new(fixture("fake_claude_tools.py"), "sonnet")
            .with_test_environment("RYNNA_TEST_SCENARIO", "reused")
            .complete(CompletionRequest {
                messages: vec![
                    Message::user("Hi"),
                    Message::assistant_with_tool_calls(vec![rynna_core::ToolCall::new(
                        "call-1",
                        "read_file",
                        json!({"path":"test.txt"}),
                    )]),
                    Message::tool("call-1", "data"),
                ],
                tools: vec![ToolDefinition::new(
                    "read_file",
                    "Read",
                    json!({"type":"object"}),
                )],
            })
            .await;
        assert!(result.is_err(), "reused tool call could execute twice");
    }

    #[tokio::test]
    async fn subscription_mode_rejects_malformed_history_before_launch() {
        use rynna_core::ToolCall;
        for messages in [
            vec![Message::tool("orphan", "data")],
            vec![Message::assistant_with_tool_calls(vec![ToolCall::new(
                "one",
                "read_file",
                json!({}),
            )])],
            vec![
                Message::assistant_with_tool_calls(vec![ToolCall::new(
                    "one",
                    "read_file",
                    json!({}),
                )]),
                Message::tool("other", "data"),
            ],
            vec![
                Message::assistant_with_tool_calls(vec![ToolCall::new(
                    "one",
                    "read_file",
                    json!({}),
                )]),
                Message::tool("one", "data"),
                Message::tool("one", "duplicate"),
            ],
        ] {
            let error = ClaudeCodeProvider::new("/nonexistent/claude", "sonnet")
                .complete(CompletionRequest {
                    messages,
                    tools: vec![],
                })
                .await
                .unwrap_err();
            assert!(error.to_string().contains("tool history"), "{error}");
        }
    }

    #[tokio::test]
    async fn subscription_mode_isolates_oauth_execution_and_streams_partial_events_once() {
        let dir = tempfile::tempdir().unwrap();
        let program = fixture("fake_claude.sh");
        let env_file = dir.path().join("env");
        let cwd_file = dir.path().join("cwd");
        let provider = ClaudeCodeProvider::new(&program, "sonnet")
            .with_test_environment("RYNNA_TEST_ENV", &env_file)
            .with_test_environment("RYNNA_TEST_CWD", &cwd_file)
            .with_test_secret_environment("ANTHROPIC_API_KEY", "must-be-removed")
            .with_test_secret_environment("ANTHROPIC_AUTH_TOKEN", "must-be-removed")
            .with_test_secret_environment("ANTHROPIC_BASE_URL", "https://gateway.example")
            .with_test_secret_environment("ANTHROPIC_PROFILE", "console-profile")
            .with_test_secret_environment("ANTHROPIC_FEDERATION_RULE_ID", "federation-rule")
            .with_test_secret_environment("CLAUDE_CODE_USE_BEDROCK", "1")
            .with_test_secret_environment("CLAUDE_CODE_USE_VERTEX", "1")
            .with_test_secret_environment("CLAUDE_CODE_USE_FOUNDRY", "1")
            .with_test_secret_environment("OPENAI_API_KEY", "must-be-removed")
            .with_test_secret_environment("AWS_SECRET_ACCESS_KEY", "must-be-removed")
            .with_test_secret_environment("CLAUDE_CODE_OAUTH_TOKEN", "keep-oauth")
            .with_test_environment("RYNNA_TEST_SCENARIO", "isolated");
        let mut deltas = vec![];
        let completion = provider
            .complete_stream(
                CompletionRequest {
                    messages: vec![Message::user("Hi")],
                    tools: vec![],
                },
                &mut |delta| deltas.push(delta.clone()),
            )
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(env_file).unwrap(),
            "unset|unset|unset|unset|unset|unset|unset|unset|unset|unset|keep-oauth"
        );
        assert_ne!(
            fs::read_to_string(cwd_file).unwrap().trim(),
            std::env::current_dir().unwrap().to_string_lossy()
        );
        assert_eq!(deltas, vec![CompletionDelta::Content("Hello".into())]);
        assert_eq!(completion.message, Message::assistant("Hello"));
    }

    #[tokio::test]
    async fn subscription_mode_requires_a_successful_result_event() {
        let program = fixture("fake_claude.sh");
        let error = ClaudeCodeProvider::new(program, "sonnet")
            .with_test_environment("RYNNA_TEST_SCENARIO", "no-result")
            .complete(CompletionRequest {
                messages: vec![Message::user("Hi")],
                tools: vec![],
            })
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("successful result event"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn subscription_mode_rejects_an_oversized_unterminated_message() {
        let program = fixture("fake_claude.sh");
        let error = ClaudeCodeProvider::new(program, "sonnet")
            .with_test_environment("RYNNA_TEST_SCENARIO", "oversized")
            .complete(CompletionRequest {
                messages: vec![Message::user("Hi")],
                tools: vec![],
            })
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("message exceeded the size limit"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn subscription_mode_rejects_an_oversized_prompt_before_starting_claude() {
        let program = fixture("fake_claude.sh");
        let error = ClaudeCodeProvider::new(program, "sonnet")
            .complete(CompletionRequest {
                messages: vec![Message::user("x".repeat(2 * 1024 * 1024))],
                tools: vec![],
            })
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("prompt exceeded the size limit"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn subscription_mode_applies_the_operation_deadline_to_stdin() {
        let program = fixture("fake_claude.sh");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ClaudeCodeProvider::new(program, "sonnet")
                .with_test_timeout(std::time::Duration::from_millis(100))
                .with_test_environment("RYNNA_TEST_SCENARIO", "blocked-stdin")
                .complete(CompletionRequest {
                    messages: vec![Message::user("x".repeat(128 * 1024))],
                    tools: vec![],
                }),
        )
        .await;

        let error = result
            .expect("the provider must enforce its own shorter deadline")
            .unwrap_err();
        assert!(
            error.to_string().contains("Claude Code timed out"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn subscription_mode_kills_a_timed_out_version_probe() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let program = fixture("fake_claude_hanging_version.sh");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ClaudeCodeProvider::new(program, "sonnet")
                .with_test_timeout(std::time::Duration::from_millis(100))
                .with_test_environment("RYNNA_TEST_PID", &pid_file)
                .complete(CompletionRequest {
                    messages: vec![Message::user("Hi")],
                    tools: vec![],
                }),
        )
        .await;

        let pid = fs::read_to_string(pid_file).unwrap();
        let running = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if running {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let error = result
            .expect("the provider must enforce its own shorter deadline")
            .unwrap_err();
        assert!(error.to_string().contains("version check timed out"));
        assert!(!running, "timed-out version probe {pid} was left running");
    }

    #[tokio::test]
    async fn subscription_mode_kills_and_reaps_after_invalid_stream_json() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let error = ClaudeCodeProvider::new(fixture("fake_claude.sh"), "sonnet")
            .with_test_environment("RYNNA_TEST_SCENARIO", "invalid-json-hang")
            .with_test_environment("RYNNA_TEST_PID", &pid_file)
            .complete(CompletionRequest {
                messages: vec![Message::user("Hi")],
                tools: vec![],
            })
            .await
            .unwrap_err();

        let pid = fs::read_to_string(pid_file).unwrap();
        let running = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if running {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .status();
        }

        assert!(error.to_string().contains("invalid stream JSON"));
        assert!(!running, "invalid-output child {pid} was left running");
    }

    #[tokio::test]
    async fn subscription_mode_kills_and_reaps_after_final_wait_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let error = ClaudeCodeProvider::new(fixture("fake_claude.sh"), "sonnet")
            .with_test_timeout(std::time::Duration::from_millis(100))
            .with_test_environment("RYNNA_TEST_SCENARIO", "success-hang")
            .with_test_environment("RYNNA_TEST_PID", &pid_file)
            .complete(CompletionRequest {
                messages: vec![Message::user("Hi")],
                tools: vec![],
            })
            .await
            .unwrap_err();

        let pid = fs::read_to_string(pid_file).unwrap();
        let running = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if running {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .status();
        }

        assert!(error.to_string().contains("timed out"));
        assert!(!running, "final-wait child {pid} was left running");
    }
}
