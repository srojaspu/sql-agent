use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::time::timeout;

#[derive(Clone)]
pub struct Ollama {
    client: Client,
    base_url: String,
    model: String,
    timeout_seconds: u64,
    temperature: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
impl Message {
    pub fn system(s: String) -> Self {
        Self {
            role: "system".into(),
            content: s,
            tool_calls: vec![],
            name: None,
        }
    }
    pub fn user(s: String) -> Self {
        Self {
            role: "user".into(),
            content: s,
            tool_calls: vec![],
            name: None,
        }
    }
    pub fn tool(name: &str, content: String) -> Self {
        Self {
            role: "tool".into(),
            content,
            tool_calls: vec![],
            name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCall {
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
    pub async fn chat(
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
            .context("Ollama devolvió HTTP error")?
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
    loop {
        let Some(start) = result.find("<think>") else {
            break;
        };
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
