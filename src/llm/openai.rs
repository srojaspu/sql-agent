use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Instant;

use super::{client, LlmProvider, Message, ToolCall, ToolDefinition, ToolFunction};
use crate::config::Config;

#[derive(Clone)]
pub struct OpenAi {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    timeout_seconds: u64,
    temperature: f32,
}

impl OpenAi {
    /// Build over the shared client from [`client::build_client`].
    ///
    /// Infallible by construction: the client arrives already built, so
    /// this constructor cannot panic.
    pub fn new(config: &Config, client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: client::base_url(config, "https://api.openai.com/v1"),
            api_key: config.llm.api_key.clone(),
            model: client::model(config, "gpt-4o-mini"),
            timeout_seconds: config.llm.timeout_s,
            temperature: config.llm.temperature,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAi {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        verbose: bool,
    ) -> Result<Message> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("Falta LLM_API_KEY para el proveedor OpenAI");
        }
        let body = json!({
            "model": self.model,
            "messages": messages.iter().map(openai_message).collect::<Vec<_>>(),
            "tools": tools.iter().map(openai_tool).collect::<Vec<_>>(),
            "temperature": self.temperature,
        });
        let started = Instant::now();
        let value = client::send_json(
            self.client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&body),
            self.timeout_seconds,
            "OpenAI",
        )
        .await?;
        let message = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .context("OpenAI no devolvió choices[0].message")?;
        let result = parse_message(message)?;
        if verbose {
            println!(
                "LLM OpenAI {:.2}s | tools={}",
                started.elapsed().as_secs_f64(),
                result.tool_calls.len()
            );
        }
        Ok(result)
    }
}

fn openai_message(message: &Message) -> Value {
    let mut value = json!({"role": message.role, "content": message.content});
    if let Some(name) = &message.name {
        value["name"] = json!(name);
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = json!(tool_call_id);
    }
    if !message.tool_calls.is_empty() {
        value["tool_calls"] =
            json!(message.tool_calls.iter().map(|call| json!({
            "id": call.id.clone().unwrap_or_else(|| call.function.name.clone()),
            "type": "function",
            "function": {
                "name": call.function.name,
                "arguments": serde_json::to_string(&call.function.arguments).unwrap_or_default()
            }
        })).collect::<Vec<_>>());
        value["content"] = Value::Null;
    }
    value
}

fn openai_tool(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.function.name,
            "description": tool.function.description,
            "parameters": tool.function.parameters
        }
    })
}

fn parse_message(value: &Value) -> Result<Message> {
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tool_calls = value
        .get("tool_calls")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .map(|call| {
            let function = call.get("function").context("Tool call OpenAI inválido")?;
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .map(serde_json::from_str)
                .transpose()
                .context("Argumentos JSON inválidos de OpenAI")?
                .unwrap_or(Value::Object(Default::default()));
            Ok(ToolCall {
                id: call.get("id").and_then(Value::as_str).map(String::from),
                function: ToolFunction {
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
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
    fn parses_openai_tool_arguments() {
        let message = parse_message(&json!({
            "content": null,
            "tool_calls": [{"id": "call_1", "function": {"name": "search_schema", "arguments": "{\"query\":\"users\"}"}}]
        }))
        .unwrap();
        assert_eq!(message.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(message.tool_calls[0].function.arguments["query"], "users");
    }
}
