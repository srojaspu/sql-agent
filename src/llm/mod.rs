mod anthropic;
mod google;
mod http;
mod ollama;
mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::config::Config;
pub use ollama::Ollama;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: String) -> Self {
        Self {
            role: "system".into(),
            content,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: String) -> Self {
        Self {
            role: "user".into(),
            content,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
        }
    }

    pub fn tool(name: &str, content: String) -> Self {
        Self {
            role: "tool".into(),
            content,
            tool_calls: vec![],
            name: Some(name.into()),
            tool_call_id: None,
        }
    }

    pub fn tool_with_call_id(name: &str, content: String, tool_call_id: Option<String>) -> Self {
        let mut message = Self::tool(name, content);
        message.tool_call_id = tool_call_id;
        message
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub r#type: &'static str,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        verbose: bool,
    ) -> Result<Message>;
}

pub fn default_provider(config: &Config) -> Arc<dyn LlmProvider> {
    match config.llm_provider.to_ascii_lowercase().as_str() {
        "openai" | "openai-compatible" => Arc::new(openai::OpenAi::new(config)),
        "google" | "gemini" => Arc::new(google::Google::new(config)),
        "anthropic" | "claude" => Arc::new(anthropic::Anthropic::new(config)),
        _ => Arc::new(Ollama::new(
            config.ollama_url.clone(),
            config.ollama_model.clone(),
            config.ollama_timeout_seconds,
            config.ollama_temperature,
            config.ollama_connect_timeout_seconds,
        )),
    }
}
