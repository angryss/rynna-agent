//! Provider model catalogs used by both settings transports. Credentials stay on the host.
use rynna_config::{ProviderKind, ResolvedProvider};
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, path::PathBuf, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
}

pub(crate) fn context_window(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1024..=100_000_000).contains(n))
}

pub async fn list_models(
    provider: ResolvedProvider,
    settings: PathBuf,
    profile: &str,
) -> Result<Vec<ProviderModel>, String> {
    tokio::time::timeout(Duration::from_secs(15), async {
        if provider.provider_kind == ProviderKind::OpenAiAccount {
            return crate::CodexAppServerProvider::for_profile(
                settings,
                profile,
                rynna_config::OPENAI_ACCOUNT_MODEL,
            )?
            .list_models()
            .await
            .map_err(|_| "Could not load OpenAI account models".to_owned());
        }
        if provider.provider_kind == ProviderKind::ClaudeSubscription {
            return Ok(Vec::new()); // Claude Code does not expose a model-list endpoint.
        }
        let key = provider
            .api_key_env
            .as_ref()
            .map(|name| {
                std::env::var(name).map_err(|_| "Provider credentials are unavailable".to_owned())
            })
            .transpose()?;
        http_models(&provider, key.as_deref()).await
    })
    .await
    .map_err(|_| "Model lookup timed out".to_owned())?
}

async fn http_models(
    provider: &ResolvedProvider,
    key: Option<&str>,
) -> Result<Vec<ProviderModel>, String> {
    let failed = || "Could not load provider models".to_owned();
    let anthropic = provider.provider_kind == ProviderKind::AnthropicMessages;
    let endpoint = format!(
        "{}{}/models",
        provider.api_base.trim_end_matches('/'),
        if anthropic { "/v1" } else { "" }
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| failed())?;
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for _ in 0..20 {
        let mut request = client.get(&endpoint);
        if anthropic {
            request = request
                .header("anthropic-version", "2023-06-01")
                .query(&[("limit", "1000")]);
            if let Some(key) = key {
                request = request.header("x-api-key", key);
            }
            if let Some(cursor) = &cursor {
                request = request.query(&[("after_id", cursor)]);
            }
        } else if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| failed())?
            .error_for_status()
            .map_err(|_| failed())?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| failed())? {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(failed());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| failed())?;
        for entry in body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(failed)?
        {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(failed)?;
            models.push(ProviderModel {
                id: id.to_owned(),
                context_window: [
                    "context_window",
                    "context_length",
                    "max_model_len",
                    "max_input_tokens",
                ]
                .iter()
                .find_map(|key| entry.get(key).and_then(context_window)),
            });
        }
        if !anthropic || body.get("has_more").and_then(Value::as_bool) != Some(true) {
            models.sort_by(|a, b| a.id.cmp(&b.id));
            models.dedup_by(|a, b| a.id == b.id);
            return Ok(models);
        }
        cursor = body
            .get("last_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !cursor
            .as_ref()
            .is_some_and(|cursor| seen.insert(cursor.clone()))
        {
            break;
        }
    }
    Err(failed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };
    fn provider(base: String, kind: ProviderKind) -> ResolvedProvider {
        ResolvedProvider {
            name: "fixture".into(),
            model: String::new(),
            provider_kind: kind,
            api_base: base,
            api_key_env: None,
            claude_program: "claude".into(),
        }
    }
    #[tokio::test]
    async fn compatible_models_use_the_configured_base_and_bearer_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/prefix/v1/models"))
            .and(header("authorization", "Bearer fixture-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":[{"id":"zeta"},{"id":"alpha"},{"id":"alpha"}]}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            http_models(
                &provider(
                    format!("{}/prefix/v1", server.uri()),
                    ProviderKind::OpenAiCompatible
                ),
                Some("fixture-key")
            )
            .await
            .unwrap(),
            vec![
                ProviderModel {
                    id: "alpha".into(),
                    context_window: None
                },
                ProviderModel {
                    id: "zeta".into(),
                    context_window: None
                }
            ]
        );
    }
    #[tokio::test]
    async fn anthropic_models_follow_pagination_with_api_headers() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .and(query_param("limit", "1000"))
            .and(header("x-api-key", "fixture-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":[{"id":"first"}],"has_more":true,"last_id":"first"}),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/v1/models"))
            .and(query_param("after_id", "first"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data":[{"id":"second"}],"has_more":false})),
            )
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            http_models(
                &provider(server.uri(), ProviderKind::AnthropicMessages),
                Some("fixture-key")
            )
            .await
            .unwrap(),
            vec![
                ProviderModel {
                    id: "first".into(),
                    context_window: None
                },
                ProviderModel {
                    id: "second".into(),
                    context_window: None
                }
            ]
        );
    }
    #[tokio::test]
    async fn failed_and_malformed_catalogs_return_safe_errors() {
        for body in [
            serde_json::json!({"error":"secret upstream detail"}),
            serde_json::json!({"data":[{}]}),
        ] {
            let server = MockServer::start().await;
            Mock::given(path("/models"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
            assert_eq!(
                http_models(&provider(server.uri(), ProviderKind::Mlx), None)
                    .await
                    .unwrap_err(),
                "Could not load provider models"
            );
        }
    }
    #[tokio::test]
    async fn catalog_limits_are_validated_and_output_limits_are_not_context_limits() {
        let server = MockServer::start().await;
        Mock::given(path("/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":[
                    {"id":"a", "context_length":128000},
                    {"id":"b", "max_input_tokens":200000, "max_tokens":4096},
                    {"id":"c", "max_model_len":8192},
                    {"id":"d", "context_window":-1},
                    {"id":"e", "context_window":100000001},
                    {"id":"f", "max_tokens":4096}
                ]})),
            )
            .mount(&server)
            .await;
        let models = http_models(
            &provider(server.uri(), ProviderKind::OpenAiCompatible),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.context_window)
                .collect::<Vec<_>>(),
            vec![Some(128000), Some(200000), Some(8192), None, None, None]
        );
    }
}
