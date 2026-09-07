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
use tokio::time::{sleep, timeout};

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

/// Retryable-status predicate shared by the backoff loop: only 429 and
/// 5xx are transient. Every other 4xx (and 2xx) fails fast without retry;
/// network/timeout errors retry in [`send_json_retry`] without a status.
pub(crate) fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// POST a JSON body with bounded retry on 429 / 5xx / network-timeout
/// using exponential backoff (800ms → 1.6s → 3.2s).
///
/// `make_request` re-constructs a fresh [`RequestBuilder`] per attempt —
/// mandatory because `RequestBuilder` is not `Clone`.
///
/// Stops after `max_retries + 1` attempts and returns the last error;
/// `max_retries = 0` means a single attempt with no retry.
pub async fn send_json_retry<F>(
    make_request: F,
    timeout_seconds: u64,
    provider: &str,
    max_retries: u8,
) -> Result<Value>
where
    F: Fn() -> RequestBuilder,
{
    // Fast path: zero retries means a single attempt through the shared
    // single-shot helper, keeping `send_json` as the canonical no-retry path.
    if max_retries == 0 {
        return send_json(make_request(), timeout_seconds, provider).await;
    }
    let mut delay = Duration::from_millis(800);
    let mut last_error = String::new();

    for attempt in 0..=max_retries {
        let request = make_request();
        let response = match timeout(Duration::from_secs(timeout_seconds.max(1)), request.send())
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                last_error = format!("{provider} error de red: {e}");
                if attempt == max_retries {
                    anyhow::bail!("{last_error}");
                }
                tracing::warn!("Reintento {}/{} para {provider}: {e}", attempt + 1, max_retries);
                sleep(delay).await;
                delay *= 2;
                continue;
            }
            Err(_) => {
                last_error = format!("Timeout HTTP de {provider}");
                if attempt == max_retries {
                    anyhow::bail!("{last_error}");
                }
                tracing::debug!(
                    "Timeout reintento {}/{} para {provider}",
                    attempt + 1,
                    max_retries
                );
                sleep(delay).await;
                delay *= 2;
                continue;
            }
        };

        let status = response.status();
        let body = response
            .text()
            .await
            .context("No se pudo leer la respuesta del proveedor LLM")?;

        if is_retryable_status(status.as_u16()) {
            last_error = format!("{provider} HTTP {status}: {}", &body[..body.len().min(200)]);
            if attempt < max_retries {
                tracing::debug!(
                    "Reintento {}/{} después de HTTP {} de {provider}: {}",
                    attempt + 1,
                    max_retries,
                    status,
                    &body[..body.len().min(100)]
                );
                sleep(delay).await;
                delay *= 2;
                continue;
            }
            anyhow::bail!("Máximo de reintentos alcanzado. Último error: {last_error}");
        }

        if !status.is_success() {
            anyhow::bail!(
                "{provider} devolvió HTTP {status}: {}",
                &body[..body.len().min(500)]
            );
        }

        return serde_json::from_str(&body)
            .with_context(|| format!("JSON inválido de {provider}"));
    }

    anyhow::bail!("Máximo de reintentos alcanzado. Último error: {last_error}");
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

    /// Minimal stub HTTP origin for retry tests: serves the queued
    /// `(status, body)` pairs in order (repeating the last one once the
    /// queue drains) and counts every hit.
    async fn spawn_stub(
        queue: Vec<(u16, String)>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub listener binds");
        let addr = listener.local_addr().expect("stub addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let queue = Arc::new(Mutex::new(queue));
        let hits_clone = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let hits = hits_clone.clone();
                let queue = queue.clone();
                tokio::spawn(async move {
                    let mut socket = socket;
                    let mut reader = tokio::io::BufReader::new(&mut socket);
                    let mut content_length = 0usize;
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line).await {
                            Ok(0) => break,
                            Ok(_) => {
                                if line.to_ascii_lowercase().starts_with("content-length:") {
                                    content_length = line
                                        .split(':')
                                        .nth(1)
                                        .unwrap_or("0")
                                        .trim()
                                        .parse()
                                        .unwrap_or(0);
                                }
                                if line == "\r\n" || line == "\n" {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let mut remaining = content_length;
                    let mut buf = vec![0u8; 8192];
                    while remaining > 0 {
                        let want = remaining.min(buf.len());
                        match reader.read(&mut buf[..want]).await {
                            Ok(0) => break,
                            Ok(n) => remaining -= n,
                            Err(_) => break,
                        }
                    }
                    hits.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = {
                        let mut q = queue.lock().unwrap();
                        if q.len() > 1 {
                            q.remove(0)
                        } else {
                            q[0].clone()
                        }
                    };
                    let reason = match status {
                        200 => "OK",
                        429 => "Too Many Requests",
                        500 => "Internal Server Error",
                        503 => "Service Unavailable",
                        _ => "Error",
                    };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let stream = reader.into_inner();
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (
            format!("http://{addr}"),
            hits,
        )
    }

    #[tokio::test]
    async fn retry_fail_then_succeed_uses_bounded_backoff() {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let (url, hits) = spawn_stub(vec![
            (503u16, "busy".to_string()),
            (503u16, "busy".to_string()),
            (200u16, r#"{"ok":true}"#.to_string()),
        ])
        .await;
        let client = reqwest::Client::new();
        let body = serde_json::json!({"probe": 1});
        let started = Instant::now();
        let value = send_json_retry(
            || client.post(url.clone()).json(&body),
            10,
            "stub",
            3,
        )
        .await
        .expect("third attempt succeeds");
        assert_eq!(value["ok"], true);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "exactly 3 attempts");
        assert!(
            started.elapsed() >= Duration::from_millis(2000),
            "two backoff delays (800ms + 1.6s) must be observed"
        );
    }

    #[tokio::test]
    async fn retry_exhaustion_returns_last_error_after_bound_plus_one() {
        use std::sync::atomic::Ordering;

        let (url, hits) = spawn_stub(vec![(429u16, "slow down".to_string())]).await;
        let client = reqwest::Client::new();
        let body = serde_json::json!({});
        let err = send_json_retry(|| client.post(url.clone()).json(&body), 10, "stub", 2)
            .await
            .expect_err("persistent 429 must exhaust");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "attempts must equal bound + 1"
        );
        assert!(
            err.to_string().contains("429"),
            "last error must surface the 429, got: {err}"
        );
    }

    #[tokio::test]
    async fn retry_zero_means_single_attempt() {
        use std::sync::atomic::Ordering;

        let (url, hits) = spawn_stub(vec![(500u16, "boom".to_string())]).await;
        let client = reqwest::Client::new();
        let body = serde_json::json!({});
        let err = send_json_retry(|| client.post(url.clone()).json(&body), 10, "stub", 0)
            .await
            .expect_err("max_retries=0 must not retry");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "zero bound ⇒ one attempt");
        assert!(
            err.to_string().contains("500"),
            "last error must surface the 500, got: {err}"
        );
    }
}
