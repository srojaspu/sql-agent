use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use super::{client, LlmProvider, Message, ToolDefinition};

#[derive(Clone)]
pub struct Ollama {
    client: Client,
    base_url: String,
    model: String,
    timeout_seconds: u64,
    temperature: f32,
    max_retries: u8,
}

#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    think: bool,
    tools: &'a [ToolDefinition],
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    options: Options,
    keep_alive: &'static str,
}
#[derive(Debug, Serialize)]
struct Options {
    temperature: f32,
}
#[derive(Debug, Deserialize)]
struct Response {
    message: Message,
    done: bool,
}

impl Ollama {
    /// Build over the shared client from [`super::client::build_client`].
    ///
    /// Infallible by construction: the client arrives already built, so
    /// this constructor cannot panic.
    pub fn new(
        url: String,
        model: String,
        timeout_seconds: u64,
        temperature: f32,
        client: Client,
        max_retries: u8,
    ) -> Self {
        Self {
            client,
            base_url: url.trim_end_matches('/').to_string(),
            model,
            timeout_seconds,
            temperature,
            max_retries,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for Ollama {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        verbose: bool,
        force_tool: bool,
    ) -> Result<Message> {
        let req = Request {
            model: &self.model,
            messages,
            stream: false,
            think: false,
            tools,
            // Force the model to emit a tool call on the first grounding step;
            // otherwise a small model (e.g. qwen3:4b) may answer in prose with
            // an invented schema instead of discovering it.
            tool_choice: if force_tool { Some("required") } else { None },
            options: Options {
                temperature: self.temperature,
            },
            keep_alive: "5m",
        };
        if verbose {
            println!("🧠 LLM → {} | esperando respuesta/tool...", self.model);
        }
        let started = Instant::now();
        let parsed = client::send_json_retry(
            || {
                self.client
                    .post(format!("{}/api/chat", self.base_url))
                    .json(&req)
            },
            self.timeout_seconds,
            "Ollama",
            self.max_retries,
        )
        .await?;
        let parsed: Response = serde_json::from_value(parsed).context("JSON inválido de Ollama")?;
        if !parsed.done {
            anyhow::bail!("Ollama no finalizó la respuesta");
        }
        if verbose {
            println!(
                "✅ LLM {:.2}s | tools={}",
                started.elapsed().as_secs_f64(),
                parsed.message.tool_calls.len()
            );
        }
        Ok(clean_message(parsed.message))
    }
}
fn clean_message(mut message: Message) -> Message {
    message.content = strip_thinking(&message.content);
    message
}
pub fn strip_thinking(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find("<think>") {
        match result.find("</think>") {
            Some(end) => result.replace_range(start..end + 8, ""),
            None => {
                result.truncate(start);
                break;
            }
        }
    }
    result
        .replace("<think>", "")
        .replace("</think>", "")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_thinking() {
        assert_eq!(strip_thinking("<think>interno</think>4"), "4");
    }
    #[test]
    fn removes_unclosed_thinking() {
        assert_eq!(strip_thinking("<think>interno"), "");
    }

    #[test]
    fn request_serializes_tool_choice_required_when_forced() {
        let messages = vec![Message::system("s".into())];
        let tools: Vec<ToolDefinition> = vec![];
        let forced = Request {
            model: "m",
            messages: &messages,
            stream: false,
            think: false,
            tools: &tools,
            tool_choice: Some("required"),
            options: Options { temperature: 0.0 },
            keep_alive: "5m",
        };
        let v = serde_json::to_value(&forced).unwrap();
        assert_eq!(v["tool_choice"], "required");

        let auto = Request {
            model: "m",
            messages: &messages,
            stream: false,
            think: false,
            tools: &tools,
            tool_choice: None,
            options: Options { temperature: 0.0 },
            keep_alive: "5m",
        };
        let v = serde_json::to_value(&auto).unwrap();
        assert!(
            v.get("tool_choice").is_none(),
            "auto must omit tool_choice, got: {v}"
        );
    }
}
