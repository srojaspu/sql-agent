use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::timeout;

use super::{LlmProvider, Message, ToolDefinition};

#[derive(Clone)]
pub struct Ollama {
    client: Client,
    base_url: String,
    model: String,
    timeout_seconds: u64,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    think: bool,
    tools: &'a [ToolDefinition],
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
    pub fn new(
        url: String,
        model: String,
        timeout_seconds: u64,
        temperature: f32,
        connect_timeout: u64,
    ) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(connect_timeout))
                .pool_idle_timeout(Duration::from_secs(300))
                .build()
                .expect("No se pudo crear HTTP client"),
            base_url: url.trim_end_matches('/').to_string(),
            model,
            timeout_seconds,
            temperature,
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
    ) -> Result<Message> {
        let req = Request {
            model: &self.model,
            messages,
            stream: false,
            think: false,
            tools,
            options: Options {
                temperature: self.temperature,
            },
            keep_alive: "5m",
        };
        if verbose {
            println!("🧠 LLM → {} | esperando respuesta/tool...", self.model);
        }
        let started = Instant::now();
        let response = timeout(
            Duration::from_secs(self.timeout_seconds),
            self.client
                .post(format!("{}/api/chat", self.base_url))
                .json(&req)
                .send(),
        )
        .await
        .context("Timeout HTTP de Ollama")??;
        let parsed: Response = response
            .error_for_status()
            .context("El proveedor LLM devolvió un error HTTP")?
            .json()
            .await
            .context("JSON inválido de Ollama")?;
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
    use super::strip_thinking;
    #[test]
    fn removes_thinking() {
        assert_eq!(strip_thinking("<think>interno</think>4"), "4");
    }
    #[test]
    fn removes_unclosed_thinking() {
        assert_eq!(strip_thinking("<think>interno"), "");
    }
}
