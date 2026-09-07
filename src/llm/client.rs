//! Shared HTTP plumbing for the LLM providers.
//!
//! Slice C folds the old `http` helpers in here so every provider builds
//! its client and reads JSON replies through one place:
//!
//! * [`build_client`] — the single shared `reqwest::Client` constructor,
//!   injected into each provider by [`crate::llm::default_provider`];
//! * [`send_json`] — timeout + status + JSON parsing shared by the
//!   OpenAI-compatible, Anthropic and Google providers;
//! * [`model`] / [`base_url`] — per-provider config resolution helpers.
//!
//! Provider-specific message/tool mapping, `strip_thinking`, timeouts and
//! URLs stay in each provider module and are untouched by this move.

use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;

use crate::error::LlmError;

/// Build the shared provider HTTP client.
///
/// `connect_timeout` is clamped to a minimum of one second (matching the
/// old `http::client` guard) and idle pooled connections are kept for
/// five minutes, exactly as before.
///
/// The only failure mode is TLS-backend initialisation inside
/// `reqwest`, surfaced as [`LlmError::Provider`] — callers never panic.
pub fn build_client(connect_timeout: Duration) -> std::result::Result<Client, LlmError> {
    Client::builder()
        .connect_timeout(connect_timeout.max(Duration::from_secs(1)))
        .pool_idle_timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| {
            LlmError::Provider(
                "http".into(),
                format!("No se pudo crear el cliente HTTP del proveedor LLM: {err}"),
            )
        })
}

/// POST a JSON body and parse the reply as JSON, with a per-request deadline.
///
/// Error texts are unchanged from the old `http::send_json` (timeout,
/// transport, non-2xx status and invalid-JSON cases keep their historical
/// Spanish messages) so provider logs stay greppable.
pub async fn send_json(
    request: RequestBuilder,
    timeout_seconds: u64,
    provider: &str,
) -> Result<Value> {
    let response = timeout(Duration::from_secs(timeout_seconds.max(1)), request.send())
        .await
        .with_context(|| format!("Timeout HTTP de {provider}"))??;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("No se pudo leer la respuesta del proveedor LLM")?;
    if !status.is_success() {
        anyhow::bail!("{provider} devolvió HTTP {status}");
    }
    serde_json::from_str(&body).with_context(|| format!("JSON inválido de {provider}"))
}

/// Resolve the model id: `LLM_MODEL` wins, otherwise the provider default.
///
/// Unchanged from the old `http::model`.
pub fn model(config: &crate::config::Config, default: &str) -> String {
    if config.llm.model.trim().is_empty() {
        default.to_string()
    } else {
        config.llm.model.clone()
    }
}

/// Resolve the endpoint base URL: `LLM_BASE_URL` wins (trailing `/`
/// trimmed), otherwise the provider default.
///
/// Unchanged from the old `http::base_url`.
pub fn base_url(config: &crate::config::Config, default: &str) -> String {
    if config.llm.base_url.trim().is_empty() {
        default.to_string()
    } else {
        config.llm.base_url.trim_end_matches('/').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn minimal_map() -> HashMap<String, String> {
        HashMap::from([
            ("DATABASE_HOST".to_string(), "localhost".to_string()),
            ("DATABASE_NAME".to_string(), "TestDB".to_string()),
            ("DATABASE_USER".to_string(), "user".to_string()),
            ("DATABASE_PASSWORD".to_string(), "pass".to_string()),
        ])
    }

    #[test]
    fn build_client_accepts_subsecond_timeout_without_panicking() {
        // The one-second floor from the old `http::client` lives here now.
        let _ = build_client(Duration::from_millis(0)).unwrap();
        let _ = build_client(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn model_prefers_llm_model_override() {
        let config = crate::config::Config::from_map(&minimal_map()).unwrap();
        assert_eq!(model(&config, "gpt-4o-mini"), "gpt-4o-mini");
        let mut map = minimal_map();
        map.insert("LLM_MODEL".to_string(), "custom".to_string());
        let config = crate::config::Config::from_map(&map).unwrap();
        assert_eq!(model(&config, "gpt-4o-mini"), "custom");
    }

    #[test]
    fn base_url_trims_trailing_slash() {
        let config = crate::config::Config::from_map(&minimal_map()).unwrap();
        assert_eq!(
            base_url(&config, "https://api.openai.com/v1"),
            "https://api.openai.com/v1"
        );
        let mut map = minimal_map();
        map.insert(
            "LLM_BASE_URL".to_string(),
            "https://proxy.local/v1/".to_string(),
        );
        let config = crate::config::Config::from_map(&map).unwrap();
        assert_eq!(base_url(&config, "x"), "https://proxy.local/v1");
    }

    #[test]
    fn is_retryable_status_retries_429_and_5xx_only() {
        // R1 predicate: 429/5xx (and network timeout in the loop) retry;
        // every other 4xx fails fast.
        for status in [429u16, 500, 502, 503, 504, 599] {
            assert!(
                is_retryable_status(status),
                "status {status} should be retryable"
            );
        }
        for status in [200u16, 201, 400, 401, 403, 404, 422] {
            assert!(
                !is_retryable_status(status),
                "status {status} should fail fast"
            );
        }
    }
}
