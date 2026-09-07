use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;

pub fn client(connect_timeout: u64) -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout.max(1)))
        .pool_idle_timeout(Duration::from_secs(300))
        .build()
        .context("No se pudo crear el cliente HTTP del proveedor LLM")
}

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

pub fn model(config: &crate::config::Config, default: &str) -> String {
    if config.llm.model.trim().is_empty() {
        default.to_string()
    } else {
        config.llm.model.clone()
    }
}

pub fn base_url(config: &crate::config::Config, default: &str) -> String {
    if config.llm.base_url.trim().is_empty() {
        default.to_string()
    } else {
        config.llm.base_url.trim_end_matches('/').to_string()
    }
}
