//! The [`LlmProvider`] abstraction shared by every LLM backend.
//!
//! Slice C moves the trait here unchanged from `llm::mod`: one async
//! `chat` call taking the shared [`crate::llm::Message`] history plus
//! [`crate::llm::ToolDefinition`] tools, returning the assistant
//! [`crate::llm::Message`]. Provider selection stays in
//! [`crate::llm::default_provider`]; error and mapping behaviour are
//! untouched by this move.

use anyhow::Result;
use async_trait::async_trait;

use super::messages::{Message, ToolDefinition};

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        verbose: bool,
        force_tool: bool,
    ) -> Result<Message>;
}
