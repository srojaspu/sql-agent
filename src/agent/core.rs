use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    config::Config,
    database::schema::{ColumnMatch, TableDetail},
    database::{DatabaseRepository, SqlServer, TableInfo},
    llm::{default_provider, LlmProvider, Message},
    security::{SecurityPolicy, SqlValidator, ValidatedSql},
};

use crate::audit::redaction::redact_content;
use crate::audit::{AuditSink, FileAuditSink};

use crate::agent::{
    prompt::SYSTEM_PROMPT,
    session::{build_history_context, is_anaphoric, Session, MAX_CHARS},
    tools,
};

use super::{
    format::{
        format_invalid_reinject, format_query_result, format_table_detail, format_table_list,
        limit_text, looks_like_sql, split_table,
    },
    memory::{
        build_schema_hint_text, dedup_key, normalize_tool_arguments, SchemaCache,
    },
    ranking::{search_with_fallback_masked, NormalizedEntry},
};

pub struct Agent {
    pub(crate) config: Config,
    pub(crate) db: Arc<dyn DatabaseRepository>,
    llm: Arc<dyn LlmProvider>,
    validator: SqlValidator,
    pub(crate) schema: Arc<RwLock<Option<SchemaCache>>>,
    audit: Arc<dyn AuditSink>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Self::with_llm(config.clone(), default_provider(&config))
    }

    pub fn with_llm(config: Config, llm: Arc<dyn LlmProvider>) -> Self {
        let db: Arc<dyn DatabaseRepository> =
            Arc::new(SqlServer::new(config.clone()));
        Self::with_repository(config, llm, db)
    }

    /// Injection seam for tests: real code uses [`Self::with_llm`] (which wraps
    /// `SqlServer`); tests pass a fake [`DatabaseRepository`].
    pub fn with_repository(
        config: Config,
        llm: Arc<dyn LlmProvider>,
        db: Arc<dyn DatabaseRepository>,
    ) -> Self {
        let policy = SecurityPolicy {
            max_sql_length: config.limits.max_sql_length,
            allowed_tables: config.policy.allowed_tables.clone(),
            block_sensitive_columns: config.policy.block_sensitive_columns,
            block_comments: config.policy.block_comments,
            allow_cte: config.policy.allow_cte,
            allow_system_tables: config.policy.allow_system_tables,
            max_joins: config.limits.max_joins,
            max_subqueries: config.limits.max_subqueries,
        };

        Self {
            db,
            llm,

            validator: SqlValidator::new(policy),

            schema: Arc::new(RwLock::new(None)),

            audit: Arc::new(FileAuditSink::new(config.audit.path.clone())),

            config,
        }
    }

    pub async fn check_db(&self) -> Result<()> {
        println!("🧪 Verificando sesión SQL Server...");

        let (db, login) = self.db.ping().await?;

        println!("✅ Base de datos: {db}");
        println!("✅ Login: {login}");

        Ok(())
    }

    pub async fn run(&self, question: &str) -> Result<String> {
        if question.trim().is_empty() {
            anyhow::bail!("Pregunta vacía");
        }

        let request_id = Uuid::new_v4().to_string();

        self.audit(
            "request",
            json!({
                "request_id": request_id,
                "question": redact_content(question)
            }),
        )
        .await?;

        /*
         * ============================================================
         * SYSTEM PROMPT — canonical from prompt.rs (single source)
         * ============================================================
         */

        let system = Message::system(format!(
            "{} Base de datos: {}.",
            SYSTEM_PROMPT, self.config.db.name
        ));

        let mut messages = vec![system, Message::user(question.to_string())];

        /*
         * ============================================================
         * AGENT LOOP
         * ============================================================
         */

        let mut tool_calls_history: Vec<String> = Vec::new();
        // Per-turn same-call dedup: repeat returns the cached result + nudge.
        let mut seen: HashMap<String, String> = HashMap::new();

        for step in 1..=self.config.limits.max_steps {
            if self.config.verbose {
                println!("\n━━━━━━━━ STEP {step}/{} ━━━━━━━━", self.config.limits.max_steps);
            }

            let tool_defs = tools::definitions();
            let reply = self
                .llm
                .chat(&messages, &tool_defs, self.config.verbose)
                .await
                .with_context(|| format!("El proveedor LLM falló en STEP {step}"))?;

            /*
             * --------------------------------------------------------
             * NO TOOL CALL
             * --------------------------------------------------------
             */

            if reply.tool_calls.is_empty() {
                let text = reply.content.trim();

                /*
                 * Algunos modelos pueden devolver SQL directamente
                 * en lugar de utilizar execute_read_query.
                 *
                 * Lo enviamos igualmente al gateway de seguridad.
                 */

                if looks_like_sql(text) {
                    if self.config.verbose {
                        println!(
                            "⚠️ SQL detectado como texto; \
                             validando y ejecutando..."
                        );
                    }

                    let result = self
                        .execute_read_tool(
                            &json!({
                                "sql": text
                            }),
                            &request_id,
                        )
                        .await?;

                    let tool_call_id = reply.tool_calls.first().and_then(|call| call.id.clone());
                    messages.push(reply);

                    messages.push(Message::tool_with_call_id(
                        "execute_read_query",
                        result,
                        tool_call_id,
                    ));

                    tool_calls_history.push("execute_read_query".to_string());

                    continue;
                }

                if !text.is_empty() {
                    self.audit(
                        "response",
                        json!({
                            "request_id": request_id,
                            "step": step,
                            "tools_used": tool_calls_history.clone()
                        }),
                    )
                    .await?;

                    return Ok(text.to_string());
                }

                messages.push(reply);

                continue;
            }

            /*
             * --------------------------------------------------------
             * TOOL CALLS
             * --------------------------------------------------------
             */

            if self.config.verbose {
                println!("🔧 Tool calls: {}", reply.tool_calls.len());
            }

            if reply.tool_calls.len() > self.config.limits.max_tool_calls_per_step {
                anyhow::bail!("Demasiadas herramientas en un mismo paso");
            }

            /*
             * MUY IMPORTANTE:
             *
             * El mensaje del assistant que contiene los tool_calls
             * debe permanecer en el historial antes de insertar
             * los resultados de las herramientas.
             */

            messages.push(reply.clone());

            for call in reply
                .tool_calls
                .iter()
                .take(self.config.limits.max_tool_calls_per_step)
            {
                let name = call.function.name.as_str();

                if self.config.verbose {
                    println!("   ↳ {name}");
                }

                // Loop guard: identical tool+args once per turn.
                let dedup_args = normalize_tool_arguments(&call.function.arguments)?;
                let dedup_k = dedup_key(name, &dedup_args);
                let result = if let Some(cached) = seen.get(&dedup_k) {
                    format!(
                        "{cached} (duplicate call deduped — retry with different arguments if needed)"
                    )
                } else {
                    let out = self.dispatch_tool(call, &request_id, None).await?;
                    seen.insert(dedup_k, out.clone());
                    out
                };

                if self.config.verbose {
                    println!("   ✓ {name} completado");
                }

                tool_calls_history.push(name.to_string());

                /*
                 * DEBUG:
                 *
                 * Permite comprobar exactamente qué recibe el LLM.
                 * Especialmente importante para modelos pequeños.
                 */

                if self.config.verbose {
                    println!("📦 TOOL RESULT [{name}]:");
                    println!("{result}");
                }

                messages.push(Message::tool_with_call_id(name, result, call.id.clone()));
            }
        }

        anyhow::bail!("Se alcanzó MAX_STEPS sin obtener una respuesta final")
    }

    /// Build LLM messages with history, schema memory hints and caps.
    /// Pure helper for testing run_with_history without side effects.
    pub fn build_messages_with_history(&self, session: &Session, question: &str) -> Vec<Message> {
        let mut system_content = format!(
            "{} Base de datos: {}.",
            SYSTEM_PROMPT, self.config.db.name
        );
        // Inject valid schema memory hints (TTL filtered, loop-guard capped)
        let hint_text = build_schema_hint_text(&session.schema_memory);
        if !hint_text.is_empty() {
            system_content.push_str("\n\nMemoria de esquema reciente:\n");
            system_content.push_str(&hint_text);
        }
        // Anaphora hint: if question is anaphoric, remind model of prior context
        if is_anaphoric(question) && !session.messages.is_empty() {
            system_content.push_str("\n\nNota: la pregunta contiene referencia anafórica (\"de esos\", \"y de esos\"); usa el historial previo para resolverla.");
        }
        let system = Message::system(system_content);
        let history_ctx = build_history_context(session, question, MAX_CHARS);
        // Prepend system
        let mut out = vec![system];
        out.extend(history_ctx);
        out
    }

    /// Run with history: multi-turn continuity, caps, schema memory, /refresh handling.
    pub async fn run_with_history(&self, session: &mut Session, question: &str) -> Result<String> {
        if question.trim().is_empty() {
            anyhow::bail!("Pregunta vacía");
        }
        let trimmed = question.trim();
        // Special commands handling (TUI commands)
        if trimmed == "/refresh" {
            // Invalidate schema cache (TTL 300s shared) and session schema_memory
            *self.schema.write().await = None;
            session.schema_memory.clear();
            // Audit
            self.audit("refresh", json!({ "session_id": session.id }))
                .await?;
            return Ok("🔄 Esquema refrescado — caché y memoria invalidados".to_string());
        }
        if trimmed == "/clear" {
            session.messages.clear();
            session.updated_at = chrono::Utc::now();
            return Ok("🧹 Historial limpiado".to_string());
        }
        if trimmed == "/history" {
            let hist: Vec<String> = session
                .messages
                .iter()
                .map(|m| {
                    format!(
                        "{}: {}",
                        m.role,
                        m.content.chars().take(200).collect::<String>()
                    )
                })
                .collect();
            if hist.is_empty() {
                return Ok("Historial vacío".to_string());
            }
            return Ok(hist.join("\n"));
        }

        let request_id = Uuid::new_v4().to_string();
        self.audit(
            "request",
            json!({
                "request_id": request_id,
                "session_id": session.id,
                "question": redact_content(question),
                "anaphoric": is_anaphoric(question)
            }),
        )
        .await?;

        // Build messages with history + caps + schema hints
        let mut messages = self.build_messages_with_history(session, question);

        // Push user message to session (caps enforced inside push)
        session.push(Message::user(question.to_string()));

        // Persist after push (best effort)
        let _ = session.persist().await;

        let mut tool_calls_history: Vec<String> = Vec::new();
        // Per-turn same-call dedup: repeat returns the cached result + nudge.
        let mut seen: HashMap<String, String> = HashMap::new();

        for step in 1..=self.config.limits.max_steps {
            if self.config.verbose {
                println!("\n━━━━━━━━ STEP {step}/{} ━━━━━━━━", self.config.limits.max_steps);
            }
            let tool_defs = tools::definitions();
            // messages already includes system + history + question; for LLM call we use the built messages clone
            // But we need to keep messages mutable for loop: we already have messages built, but we need to update it each iteration
            let reply = self
                .llm
                .chat(&messages, &tool_defs, self.config.verbose)
                .await
                .with_context(|| format!("El proveedor LLM falló en STEP {step}"))?;

            if reply.tool_calls.is_empty() {
                let text = reply.content.trim();
                if looks_like_sql(text) {
                    let result = self
                        .execute_read_tool(&json!({ "sql": text }), &request_id)
                        .await?;
                    let assistant_msg = reply.clone();
                    session.push(assistant_msg.clone());
                    messages.push(assistant_msg);
                    let tool_msg = Message::tool_with_call_id(
                        "execute_read_query",
                        result.clone(),
                        reply.tool_calls.first().and_then(|call| call.id.clone()),
                    );
                    session.push(tool_msg.clone());
                    messages.push(tool_msg);
                    tool_calls_history.push("execute_read_query".to_string());
                    continue;
                }
                if !text.is_empty() {
                    // Push assistant final response to session
                    session.push(reply.clone());
                    messages.push(reply.clone());
                    self.audit(
                        "response",
                        json!({
                            "request_id": request_id,
                            "session_id": session.id,
                            "step": step,
                            "tools_used": tool_calls_history.clone()
                        }),
                    )
                    .await?;
                    let _ = session.persist().await;
                    return Ok(text.to_string());
                }
                session.push(reply.clone());
                messages.push(reply);
                continue;
            }

            if reply.tool_calls.len() > self.config.limits.max_tool_calls_per_step {
                anyhow::bail!("Demasiadas herramientas en un mismo paso");
            }

            // Must keep assistant tool_calls message before results
            session.push(reply.clone());
            messages.push(reply.clone());

            for call in reply
                .tool_calls
                .iter()
                .take(self.config.limits.max_tool_calls_per_step)
            {
                let name = call.function.name.as_str();
                if self.config.verbose {
                    println!("   ↳ {name}");
                }
                // For schema-related tools, update schema_memory before/after
                // Loop guard: identical tool+args once per turn.
                let dedup_args = normalize_tool_arguments(&call.function.arguments)?;
                let dedup_k = dedup_key(name, &dedup_args);
                let result = if let Some(cached) = seen.get(&dedup_k) {
                    format!(
                        "{cached} (duplicate call deduped — retry with different arguments if needed)"
                    )
                } else {
                    let out = self
                        .dispatch_tool(call, &request_id, Some(&mut *session))
                        .await?;
                    seen.insert(dedup_k, out.clone());
                    out
                };
                if self.config.verbose {
                    println!("   ✓ {name} completado");
                    println!("📦 TOOL RESULT [{name}]:\n{result}");
                }
                tool_calls_history.push(name.to_string());
                let tool_msg = Message::tool_with_call_id(name, result, call.id.clone());
                session.push(tool_msg.clone());
                messages.push(tool_msg);
            }
            let _ = session.persist().await;
        }

        anyhow::bail!("Se alcanzó MAX_STEPS sin obtener una respuesta final")
    }

    /*
     * ================================================================
     * TOOL DISPATCH lives in `agent::dispatcher` (single unified
     * `dispatch_tool` for both loops; no duplicated match arms).
     * ================================================================
     */

    /*
     * ================================================================
     * SEARCH SCHEMA
     * ================================================================
     */

    /// Single-ranking search: one cache snapshot + one masked ranking pass.
    /// Returns (formatted_output, matched) so history dispatch reuses `matched`
    /// for memory without a second `search_with_fallback` call. The zero-hit
    /// top list comes from the same ranking pass (no re-rank loop).
    pub(crate) async fn search_schema_ranked(&self, query_raw: &str) -> Result<(String, Vec<TableInfo>)> {
        let snapshot = self.cached_schema().await?;
        let tables: &[TableInfo] = &snapshot.tables;
        let norm: &[NormalizedEntry] = &snapshot.normalized;
        // Allowlist mask avoids cloning the full table Vec into `allowed`.
        let mask: Vec<bool> = tables
            .iter()
            .map(|t| self.table_allowed(&t.schema, &t.table))
            .collect();
        let allowed_count = mask.iter().filter(|&&b| b).count();

        let (matched, did_you_mean, top) = search_with_fallback_masked(
            query_raw,
            tables,
            norm,
            Some(&mask),
            self.config.limits.max_schema_results,
        );

        if self.config.verbose {
            println!("🔎 search_schema → {} coincidencias", matched.len());
        }

        let mut output = String::new();

        output.push_str(&format!("✓ TABLAS ENCONTRADAS: {}\n", matched.len()));

        if matched.is_empty() {
            output.push_str("No se encontraron tablas.\n");
            if let Some(suggestion) = did_you_mean.clone() {
                if !top.is_empty() {
                    output.push_str("\nSugerencias (top):\n");
                    for t in &top {
                        output.push_str(&format!("  • {}.{}\n", t.schema, t.table));
                    }
                    output.push_str(&format!(
                        "\nDid you mean: {}.{} ?\n",
                        suggestion.schema, suggestion.table
                    ));
                    output.push_str(
                        "→ Usa search_schema con término corregido o describe_table \
                         con el nombre calificado exacto (no inventes dbo.*).\n",
                    );
                }
                output.push_str(
                    "\n⚠️ NO inventes nombres de tabla. Usa EXCLUSIVAMENTE nombres \
                     de la lista Sugerencias o del resultado de search_schema.\n",
                );
                output.push_str(
                    "Si el resultado es 0, NO llames a describe_table ni \
                     execute_read_query con nombres inventados.\n",
                );
            } else if allowed_count == 0 {
                output.push_str("→ No hay tablas visibles para tu filtro.\n");
            }
        } else {
            for table in &matched {
                output.push_str(&format!("  • {}.{}\n", table.schema, table.table));
            }
            output.push_str(
                "\n→ SIGUIENTE PASO: Usa describe_table con el nombre calificado \
                 EXACTO de la lista anterior (copia literal), luego execute_read_query.\n",
            );
            output.push_str(
                "⚠️ No inventes dbo.* ni asumas plural/singular; \
                 usa solo nombres devueltos por search_schema.\n",
            );
        }

        Ok((
            limit_text(&output, self.config.limits.max_tool_result_chars),
            matched,
        ))
    }

    /*
     * ================================================================
     * DESCRIBE TABLE
     * ================================================================
     */

    /// Describe once and return (formatted_output, columns): history dispatch
    /// reuses `columns` for schema memory instead of a second `describe_table`
    /// DB call. Single `describe_table_full` fetch, same format as before.
    pub(crate) async fn describe_table_tool_with_detail(
        &self,
        args: &Value,
    ) -> Result<(String, Vec<crate::database::ColumnInfo>)> {
        let table = args
            .get("table")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();

        if table.is_empty() {
            anyhow::bail!("Falta table");
        }

        let (schema, name) = split_table(table);

        if !self.table_allowed(&schema, &name) {
            anyhow::bail!("Tabla no permitida: {table}");
        }

        if self.config.verbose {
            println!("📐 Describiendo {}.{}", schema, name);
        }

        let detail: TableDetail = self
            .db
            .describe_table_full(&format!("{}.{}", schema, name))
            .await?;

        Ok((
            limit_text(
                &format_table_detail(&schema, &name, &detail),
                self.config.limits.max_tool_result_chars,
            ),
            detail.columns,
        ))
    }

    async fn describe_table_tool(&self, args: &Value) -> Result<String> {
        Ok(self.describe_table_tool_with_detail(args).await?.0)
    }

    /*
     * ================================================================
     * DISCOVERY TOOLS (SG-1)
     * ================================================================
     */

    /// List every visible table and view (SG-1). Cached via `cached_tables`.
    pub(crate) async fn list_tables_tool(&self) -> Result<String> {
        let tables = self.cached_tables().await?;
        let allowed: Vec<TableInfo> = tables
            .iter()
            .filter(|t| self.table_allowed(&t.schema, &t.table))
            .cloned()
            .collect();
        Ok(limit_text(
            &format_table_list(&allowed),
            self.config.limits.max_tool_result_chars,
        ))
    }

    /// Structured column search filtered by the allowlist (SG-1).
    pub(crate) async fn search_columns_data(&self, query: &str) -> Result<Vec<ColumnMatch>> {
        if query.trim().is_empty() {
            anyhow::bail!("Falta query");
        }
        let matches = self.db.search_columns(query).await?;
        Ok(matches
            .into_iter()
            .filter(|m| self.table_allowed(&m.schema, &m.table))
            .collect())
    }

    /*
     * ================================================================
     * EXECUTE READ QUERY
     * ================================================================
     */

    pub(crate) async fn execute_read_tool(&self, args: &Value, request_id: &str) -> Result<String> {
        let sql = args.get("sql").and_then(Value::as_str).unwrap_or("").trim();

        if sql.is_empty() {
            anyhow::bail!("Falta sql");
        }

        if self.config.verbose {
            println!("🔐 Validando SQL...");
        }

        // Security seam: only validator-approved SQL flows to the DB, by value.
        let validated = match ValidatedSql::parse(&self.validator, sql) {
            Ok(v) => v,
            Err(crate::error::ValidationBlocked::Blocked(msg)) => {
                tracing::warn!("SQL validation blocked: {msg}");
                let out = format!(
                    "❌ Consulta bloqueada por política de seguridad: {msg}\n\
                     Ajusta tu consulta para cumplir la política (ej: solo lectura SELECT, sin comentarios, \
                     máximo {} JOINs y únicamente tablas y columnas autorizadas).",
                    self.config.limits.max_joins
                );
                return Ok(limit_text(&out, self.config.limits.max_tool_result_chars));
            }
        };

        if self.config.verbose {
            println!("✅ SQL válido");
        }

        self.audit(
            "sql_approved",
            json!({
                "request_id": request_id,
                "sql": if self.config.audit.capture_sql {
                    json!(redact_content(validated.as_str()))
                } else {
                    json!("[REDACTED]")
                }
            }),
        )
        .await?;

        if self.config.verbose {
            println!("🗄️ Ejecutando consulta...");
        }

        // Save the approved text for logging before moving it into the DB call.
        let sql_for_log = validated.as_str().to_owned();
        let result = match self.db.execute_read(validated).await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if msg.to_ascii_lowercase().contains("invalid object name") {
                    // Re-inject candidates (top 17) for self-correction within MAX_STEPS.
                    // Clone at most 17 entries; the cache itself stays a shared Arc.
                    let tables = self.cached_tables().await.unwrap_or_default();
                    let allowed: Vec<TableInfo> = tables
                        .iter()
                        .filter(|t| self.table_allowed(&t.schema, &t.table))
                        .take(17)
                        .cloned()
                        .collect();
                    let out = format_invalid_reinject(&allowed, &msg);
                    tracing::warn!(
                        "Invalid object name re-injected {} candidates for sql: {}",
                        allowed.len().min(17),
                        sql_for_log
                    );
                    return Ok(limit_text(&out, self.config.limits.max_tool_result_chars));
                } else {
                    tracing::warn!("SQL execution error returned for self-correction: {msg}");
                    let out = format!(
                        "❌ Error de SQL Server: {msg}\n\
                         Analiza el error. Si falló por nombre de columna inválido o cláusula GROUP BY, \
                         verifica las columnas reales con describe_table o search_columns. \
                         Corrige la consulta y ejecútala nuevamente."
                    );
                    return Ok(limit_text(&out, self.config.limits.max_tool_result_chars));
                }
            }
        };

        if self.config.verbose {
            println!(
                "📊 {} fila(s){}",
                result.row_count,
                if result.truncated { " [LIMITADO]" } else { "" }
            );
        }

        /*
         * Formatear resultados de forma clara para LLM pequeño
         * (redaction by header lives inside format_query_result).
         */
        let formatted = format_query_result(&result.rows, result.truncated);

        Ok(limit_text(&formatted, self.config.limits.max_tool_result_chars))
    }

    /*
     * ================================================================
     * AUDIT
     * ================================================================
     */

    async fn audit(&self, event: &str, payload: Value) -> Result<()> {
        if self.config.audit.enabled {
            self.audit.write(event, payload).await?;
        }

        Ok(())
    }

    /*
     * ================================================================
     * TUI HELPERS (cache + allowlist live in `agent::memory`)
     * ================================================================
     */

    /// List tables for TUI /tables command — cached, no LLM, filtered by allowlist
    pub async fn tui_list_tables(&self) -> Result<String> {
        let tables = self.cached_tables().await?;
        let allowed: Vec<_> = tables
            .iter()
            .filter(|t| self.table_allowed(&t.schema, &t.table))
            .collect();
        if allowed.is_empty() {
            Ok("No hay tablas visibles para tu filtro.".to_string())
        } else {
            let mut out = format!("Tablas disponibles ({}):\n", allowed.len());
            for t in allowed.iter().take(self.config.limits.max_schema_results) {
                out.push_str(&format!("  • {}.{}\n", t.schema, t.table));
            }
            Ok(out)
        }
    }

    /// Describe table for TUI /describe command — no LLM
    pub async fn tui_describe(&self, table: &str) -> Result<String> {
        let args = json!({"table": table});
        self.describe_table_tool(&args).await
    }

    /// Refresh schema cache for TUI /refresh — clears both SchemaCache and returns count
    pub async fn refresh_cache(&self) {
        *self.schema.write().await = None;
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_columns_data_rejects_empty_query_without_db() {
        let agent = Agent::new(dummy_config());
        assert!(agent.search_columns_data("   ").await.is_err());
    }

    // ===== Task 2.4 run_with_history helpers =====
    fn dummy_config() -> crate::config::Config {
        crate::config::Config {
            db: crate::config::DbConfig {
                host: "localhost".into(),
                port: 1433,
                name: "TestDB".into(),
                user: "user".into(),
                password: "pass".into(),
                trust_cert: true,
            },
            llm: crate::config::LlmConfig {
                provider: "ollama".into(),
                model: String::new(),
                api_key: String::new(),
                base_url: String::new(),
                ollama_url: "http://127.0.0.1:11434".into(),
                ollama_model: "qwen3:4b".into(),
                timeout_s: 120,
                connect_timeout_s: 5,
                temperature: 0.0,
                max_retries: 3,
            },
            policy: crate::config::PolicyConfig {
                allowed_tables: vec![],
                block_sensitive_columns: true,
                block_comments: true,
                allow_cte: true,
                allow_system_tables: false,
            },
            limits: crate::config::LimitsConfig {
                max_steps: 8,
                max_sql_length: 10_000,
                max_rows: 100,
                schema_cache_s: 300,
                query_timeout_s: 30,
                max_concurrent_queries: 1,
                max_joins: 5,
                max_subqueries: 5,
                max_schema_results: 20,
                max_tool_result_chars: 20_000,
                max_tool_calls_per_step: 10,
            },
            audit: crate::config::AuditConfig {
                enabled: false,
                path: "logs/test-audit.jsonl".into(),
                capture_sql: false,
            },
            verbose: false,
        }
    }

    #[test]
    fn build_messages_with_history_anaphora_includes_history() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        // Prior turn: user asked about usuarios, tool returned 2 rows
        session.push(crate::llm::Message::user("cuantos usuarios hay".into()));
        session.push(crate::llm::Message::tool(
            "execute_read_query",
            "✓ RESULTADOS (2 filas) Fila 1: Usuario=Juan activo=1 Fila 2: Usuario=Ana activo=0"
                .into(),
        ));
        let msgs = agent.build_messages_with_history(&session, "y de esos cuantos activos");
        // System + history + new question => should contain prior tool result and new question
        let joined: String = msgs
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            joined.contains("cuantos usuarios hay"),
            "history should be injected, got: {joined}"
        );
        assert!(
            joined.contains("y de esos cuantos activos"),
            "new anaphoric question should be present"
        );
        assert!(
            joined.contains("Fila 1"),
            "tool result should be retained for anaphora resolution"
        );
        // System should contain anaphoric hint
        assert!(
            msgs[0].content.to_ascii_lowercase().contains("anafórica")
                || msgs[0].content.contains("de esos"),
            "system should hint anaphoric usage"
        );
    }

    #[test]
    fn build_messages_with_history_non_anaphoric_still_injects_history() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        session.push(crate::llm::Message::user("muestra productos".into()));
        let msgs = agent.build_messages_with_history(&session, "cuantos pedidos hay");
        let joined: String = msgs
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        // Both prior and new should be present (multi-turn continuity)
        assert!(joined.contains("muestra productos"));
        assert!(joined.contains("cuantos pedidos hay"));
        // Should NOT contain anaphoric note for non-anaphoric
        assert!(
            !msgs[0].content.to_ascii_lowercase().contains("anafórica")
                || !crate::agent::session::is_anaphoric("cuantos pedidos hay")
        );
    }

    #[test]
    fn build_messages_caps_respected_via_session() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        for i in 0..50 {
            session.push(crate::llm::Message::user(format!("msg {i}")));
        }
        // Session itself should be capped at 40
        assert_eq!(session.messages.len(), 40);
        let msgs = agent.build_messages_with_history(&session, "pregunta final");
        // Messages = system + history (40) + question (1) but capped to 40 history -> should be <= 41 + system
        // System is 1, history_ctx is capped to 40, so total <= 41 + maybe truncated
        assert!(
            msgs.len() <= 42,
            "should respect 40 cap + system + question, got {}",
            msgs.len()
        );
        // Oldest msg 0 should have been evicted
        let joined: String = msgs
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!joined.contains("msg 0 "), "oldest should be evicted");
    }

    #[test]
    fn build_messages_injects_schema_memory_hints_when_valid() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        let tbl = crate::database::TableInfo {
            schema: "dbo".into(),
            table: "Usuario".into(),
            table_type: "BASE TABLE".into(),
        };
        let entry = crate::database::schema::SchemaMemoryEntry::new(
            tbl,
            vec![],
            vec!["usuarios".into()],
            300,
        );
        session.schema_memory.insert("dbo.Usuario".into(), entry);
        let msgs = agent.build_messages_with_history(&session, "cuantos usuarios");
        assert!(
            msgs[0].content.contains("dbo.Usuario") && msgs[0].content.contains("usuarios"),
            "system should contain schema memory hint, got: {}",
            msgs[0].content
        );
    }

    #[test]
    fn build_messages_expired_schema_memory_not_injected() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        let tbl = crate::database::TableInfo {
            schema: "dbo".into(),
            table: "Usuario".into(),
            table_type: "BASE TABLE".into(),
        };
        let mut entry = crate::database::schema::SchemaMemoryEntry::new(
            tbl,
            vec![],
            vec!["usuarios".into()],
            1,
        );
        // Make expired
        entry.last_used = chrono::Utc::now() - chrono::Duration::seconds(10);
        session.schema_memory.insert("dbo.Usuario".into(), entry);
        let msgs = agent.build_messages_with_history(&session, "cuantos usuarios");
        assert!(
            !msgs[0].content.contains("dbo.Usuario"),
            "expired entry should not be injected, got: {}",
            msgs[0].content
        );
    }

    #[tokio::test]
    async fn run_with_history_refresh_clears_memory_and_cache() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        let tbl = crate::database::TableInfo {
            schema: "dbo".into(),
            table: "Usuario".into(),
            table_type: "BASE TABLE".into(),
        };
        let entry = crate::database::schema::SchemaMemoryEntry::new(
            tbl,
            vec![],
            vec!["usuarios".into()],
            300,
        );
        session.schema_memory.insert("dbo.Usuario".into(), entry);
        // Put dummy cache
        {
            let mut guard = agent.schema.write().await;
            *guard = Some(SchemaCache {
                expires_at: std::time::Instant::now() + std::time::Duration::from_secs(300),
                tables: Arc::new(vec![]),
                normalized: Arc::new(vec![]),
            });
        }
        let res = agent
            .run_with_history(&mut session, "/refresh")
            .await
            .unwrap();
        assert!(res.contains("refrescado") || res.contains("Refrescado") || res.contains("🔄"));
        assert!(
            session.schema_memory.is_empty(),
            "schema_memory should be cleared on /refresh"
        );
        assert!(
            agent.schema.read().await.is_none(),
            "schema cache should be cleared on /refresh"
        );
    }

    #[tokio::test]
    async fn run_with_history_clear_empties_messages() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        session.push(crate::llm::Message::user("hola".into()));
        let res = agent
            .run_with_history(&mut session, "/clear")
            .await
            .unwrap();
        assert!(res.contains("limpiado") || res.contains("Historial"));
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn run_with_history_empty_question_bails() {
        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();
        let res = agent.run_with_history(&mut session, "   ").await;
        assert!(res.is_err());
    }

    // ===== slice D step 2: unified-dispatch parity (lock-in guard) =====
    // Same tool-call input through the unified dispatcher with and without a
    // session must produce identical output; the session path additionally
    // grounds memory. The pre-unification probe showed both twin paths already
    // agreed on outputs (divergence was session side-effects only), so this
    // guard pins the fused behavior. Only DB-free arms run here; DB-touching
    // arms share the same callees on both paths by construction.
    #[tokio::test]
    async fn dispatch_unified_parity_none_vs_session() {
        use crate::llm::{ToolCall, ToolFunction};

        fn call(name: &str, args: serde_json::Value) -> ToolCall {
            ToolCall {
                id: None,
                function: ToolFunction {
                    name: name.to_string(),
                    arguments: args,
                },
            }
        }

        let agent = Agent::new(dummy_config());
        let mut session = crate::agent::session::Session::new();

        // Every case below resolves without touching the DB.
        let cases: Vec<(&str, serde_json::Value)> = vec![
            // Policy-blocked SQL short-circuits before audit/DB.
            (
                "execute_read_query",
                serde_json::json!({"sql": "DELETE FROM dbo.Usuario"}),
            ),
            // Missing-arg bails precede any DB call.
            ("execute_read_query", serde_json::json!({"sql": ""})),
            ("execute_read_query", serde_json::json!({})),
            ("describe_table", serde_json::json!({"table": ""})),
            ("search_columns", serde_json::json!({"query": ""})),
            ("search_columns", serde_json::json!({})),
            // Unknown tool never reaches the DB.
            ("herramienta_fantasma", serde_json::json!({})),
        ];

        for (name, args) in cases {
            let c = call(name, args);
            let without = agent.dispatch_tool(&c, "req-parity", None).await;
            let with = agent
                .dispatch_tool(&c, "req-parity", Some(&mut session))
                .await;
            match (without, with) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a, b, "dispatch output diverged for tool '{name}'")
                }
                (Err(a), Err(b)) => assert_eq!(
                    a.to_string(),
                    b.to_string(),
                    "dispatch error diverged for tool '{name}'"
                ),
                (a, b) => panic!("dispatch ok/err diverged for tool '{name}': {a:?} vs {b:?}"),
            }
        }
        // Bail-out arms ground nothing: session memory stays empty.
        assert!(
            session.schema_memory.is_empty(),
            "unified dispatch must not ground memory on bail-out arms"
        );
    }

}
