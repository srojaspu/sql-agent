use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Instant;

use super::{http, LlmProvider, Message, ToolCall, ToolDefinition, ToolFunction};
use crate::config::Config;

#[derive(Clone)]
pub struct Anthropic {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    timeout_seconds: u64,
    temperature: f32,
}

impl Anthropic {
    pub fn new(config: &Config) -> Self {
        Self {
            client: http::client(config.ollama_connect_timeout_seconds)
                .expect("No se pudo crear HTTP client"),
            base_url: http::base_url(config, "https://api.anthropic.com/v1"),
            api_key: config.llm_api_key.clone(),
            model: http::model(config, "claude-3-5-haiku-latest"),
            timeout_seconds: config.ollama_timeout_seconds,
            temperature: config.ollama_temperature,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for Anthropic {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        verbose: bool,
    ) -> Result<Message> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("Falta LLM_API_KEY para el proveedor Anthropic");
        }
        let system = messages
            .iter()
            .find(|message| message.role == "system")
            .map(|message| message.content.clone());
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system,
            "messages": messages.iter().filter(|message| message.role != "system").map(anthropic_message).collect::<Vec<_>>(),
            "tools": tools.iter().map(anthropic_tool).collect::<Vec<_>>(),
            "temperature": self.temperature,
        });
        let started = Instant::now();
        let value = http::send_json(
            self.client
                .post(format!("{}/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body),
            self.timeout_seconds,
            "Anthropic",
        )
        .await?;
        let blocks = value
            .get("content")
            .and_then(Value::as_array)
            .context("Anthropic no devolvió content")?;
        let result = parse_blocks(blocks)?;
        if verbose {
            println!(
                "LLM Anthropic {:.2}s | tools={}",
                started.elapsed().as_secs_f64(),
                result.tool_calls.len()
            );
        }
        Ok(result)
    }
}

fn anthropic_message(message: &Message) -> Value {
    if message.role == "tool" {
        return json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": message.tool_call_id.clone().unwrap_or_else(|| message.name.clone().unwrap_or_default()), "content": message.content}]
        });
    }
    let mut content = Vec::new();
    if !message.content.is_empty() {
        content.push(json!({"type": "text", "text": message.content}));
    }
    for call in &message.tool_calls {
        content.push(json!({
            "type": "tool_use",
            "id": call.id.clone().unwrap_or_else(|| call.function.name.clone()),
            "name": call.function.name,
            "input": call.function.arguments
        }));
    }
    json!({"role": if message.role == "assistant" { "assistant" } else { "user" }, "content": content})
}

fn anthropic_tool(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.function.name,
        "description": tool.function.description,
        "input_schema": tool.function.parameters
    })
}

fn parse_blocks(blocks: &[Value]) -> Result<Message> {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push_str(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block.get("id").and_then(Value::as_str).map(String::from),
                function: ToolFunction {
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                },
            }),
            _ => {}
        }
    }
    Ok(Message {
        role: "assistant".into(),
        content,
        tool_calls,
        name: None,
        tool_call_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anthropic_tool_use() {
        let message = parse_blocks(&[json!({"type": "tool_use", "id": "tool_1", "name": "search_schema", "input": {"query": "users"}})]).unwrap();
        assert_eq!(message.tool_calls[0].id.as_deref(), Some("tool_1"));
        assert_eq!(message.tool_calls[0].function.arguments["query"], "users");
    }
}
