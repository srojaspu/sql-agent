//! LLM provider settings.
//!
//! Sourced from `LLM_*` / `OLLAMA_*` environment variables by
//! [`crate::config::loader`]. The generic `LLM_*` knobs select the provider
//! while the `OLLAMA_*` knobs carry timeouts and sampling shared by every
//! provider implementation. Defaults match the pre-split `config.rs`.

/// LLM provider settings (`LLM_*` + shared `OLLAMA_*` knobs).
#[derive(Clone, Debug)]
pub struct LlmConfig {
    /// Provider id: `ollama` (default), `openai` / `openai-compatible`,
    /// `google` / `gemini`, `anthropic` / `claude`.
    ///
    /// Env: `LLM_PROVIDER` (default `"ollama"`).
    pub provider: String,

    /// Provider-specific model id; empty means "provider default"
    /// (resolved per provider, e.g. `gpt-4o-mini` for OpenAI).
    ///
    /// Env: `LLM_MODEL` (default `""`).
    pub model: String,

    /// Single generic API key for every non-Ollama provider.
    ///
    /// Env: `LLM_API_KEY` (default `""`).
    ///
    /// Vendor-specific keys (`OPENAI_API_KEY`, `GOOGLE_API_KEY`,
    /// `ANTHROPIC_API_KEY`) are rejected by the loader: use this key plus
    /// `LLM_PROVIDER` instead.
    pub api_key: String,

    /// Override for OpenAI-compatible endpoints; empty means the provider
    /// default (e.g. `https://api.openai.com/v1`).
    ///
    /// Env: `LLM_BASE_URL` (default `""`).
    pub base_url: String,

    /// Ollama server base URL.
    ///
    /// Env: `OLLAMA_URL` (default `"http://127.0.0.1:11434"`).
    pub ollama_url: String,

    /// Ollama model tag.
    ///
    /// Env: `OLLAMA_MODEL` (default `"qwen3:4b"`).
    pub ollama_model: String,

    /// Per-request LLM deadline in seconds.
    ///
    /// Env: `OLLAMA_TIMEOUT_SECONDS` (default `120`).
    pub timeout_s: u64,

    /// TCP connect timeout in seconds for provider HTTP clients.
    ///
    /// Env: `OLLAMA_CONNECT_TIMEOUT_SECONDS` (default `5`).
    pub connect_timeout_s: u64,

    /// Sampling temperature forwarded to every provider.
    ///
    /// Env: `OLLAMA_TEMPERATURE` (default `"0.0"`, parse error message
    /// `"OLLAMA_TEMPERATURE inválido"` preserved by the loader).
    pub temperature: f32,
}
