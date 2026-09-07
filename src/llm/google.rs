use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Instant;

use super::{http, LlmProvider, Message, ToolCall, ToolDefinition, ToolFunction};
use crate::config::Config;

#[derive(Clone)]
pub struct Google {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    timeout_seconds: u64,
    temperature: f32,
}

impl Google {
    pub fn new(config: &Config) -> Self {
        Self {
            client: http::client(config.llm.connect_timeout_s)
                .expect("No se pudo crear HTTP client"),
            base_url: http::base_url(config, "https://generativelanguage.googleapis.com/v1beta"),
            api_key: config.llm.api_key.clone(),
            model: http::model(config, "gemini-2.0-flash"),
            timeout_seconds: config.llm.timeout_s,
            temperature: config.llm.temperature,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for Google {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        verbose: bool,
    ) -> Result<Message> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("Falta LLM_API_KEY para el proveedor Google");
        }
        let system_instruction = messages
            .iter()
            .find(|message| message.role == "system")
            .map(|message| json!({"parts": [{"text": message.content}]}));
        let body = json!({
            "systemInstruction": system_instruction,
            "contents": messages.iter().filter(|message| message.role != "system").map(google_content).collect::<Vec<_>>(),
            "tools": if tools.is_empty() { Value::Null } else { json!([{ "functionDeclarations": tools.iter().map(google_tool).collect::<Vec<_>>() }]) },
            "generationConfig": {"temperature": self.temperature}
        });
        let started = Instant::now();
        let value = http::send_json(
            self.client
                .post(format!(
                    "{}/models/{}:generateContent",
                    self.base_url, self.model
                ))
                .query(&[("key", &self.api_key)])
                .json(&body),
            self.timeout_seconds,
            "Google Gemini",
        )
        .await?;
        let parts = value
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .context("Google no devolvió candidates[0].content.parts")?;
        let result = parse_parts(parts)?;
        if verbose {
            println!(
                "LLM Google {:.2}s | tools={}",
                started.elapsed().as_secs_f64(),
                result.tool_calls.len()
            );
        }
        Ok(result)
    }
}

fn google_content(message: &Message) -> Value {
    let role = if message.role == "assistant" {
        "model"
    } else {
        "user"
    };
    let mut parts = Vec::new();
    if !message.content.is_empty() {
        parts.push(json!({"text": message.content}));
    }
    if message.role == "tool" {
        parts = vec![json!({
            "functionResponse": {
                "name": message.name.clone().unwrap_or_default(),
                "response": serde_json::from_str::<Value>(&message.content).unwrap_or(json!({"result": message.content}))
            }
        })];
    }
    json!({"role": role, "parts": parts})
}

fn google_tool(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.function.name,
        "description": tool.function.description,
        "parameters": tool.function.parameters
    })
}

fn parse_parts(parts: &[Value]) -> Result<Message> {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            content.push_str(text);
        }
        if let Some(call) = part.get("functionCall") {
            tool_calls.push(ToolCall {
                id: None,
                function: ToolFunction {
                    name: call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    arguments: call.get("args").cloned().unwrap_or_else(|| json!({})),
                },
            });
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
    fn parses_gemini_function_call() {
        let message = parse_parts(&[
            json!({"functionCall": {"name": "search_schema", "args": {"query": "users"}}}),
        ])
        .unwrap();
        assert_eq!(message.tool_calls[0].function.name, "search_schema");
        assert_eq!(message.tool_calls[0].function.arguments["query"], "users");
    }
}
