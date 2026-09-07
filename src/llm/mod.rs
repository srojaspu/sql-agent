mod anthropic;
mod client;
mod google;
mod messages;
mod ollama;
mod openai;
mod provider;

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;

use crate::config::Config;
pub use client::build_client;
pub use messages::{FunctionDefinition, Message, ToolCall, ToolDefinition, ToolFunction};
pub use ollama::Ollama;
pub use provider::LlmProvider;

pub fn default_provider(config: &Config) -> Arc<dyn LlmProvider> {
    // One shared client for whichever provider is selected. `build_client`
    // only fails on TLS-backend init, so fall back to a default client
    // rather than panic (no `.expect` under `llm`).
    let client = build_client(Duration::from_secs(config.llm.connect_timeout_s.max(1)))
        .unwrap_or_else(|_| Client::new());
    match config.llm.provider.to_ascii_lowercase().as_str() {
        "openai" | "openai-compatible" => Arc::new(openai::OpenAi::new(config, client)),
        "google" | "gemini" => Arc::new(google::Google::new(config, client)),
        "anthropic" | "claude" => Arc::new(anthropic::Anthropic::new(config, client)),
        _ => Arc::new(Ollama::new(
            config.llm.ollama_url.clone(),
            config.llm.ollama_model.clone(),
            config.llm.timeout_s,
            config.llm.temperature,
            client,
            config.llm.max_retries,
        )),
    }
}
