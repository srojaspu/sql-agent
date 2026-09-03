use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    audit,
    config::Config,
    database::schema::{upsert_schema_memory, ColumnMatch, TableDetail},
    database::{SqlServer, TableInfo},
    llm::{Message, Ollama, ToolCall},
    security::{SecurityPolicy, SqlValidator},
};

use crate::agent::{
    prompt::SYSTEM_PROMPT,
    session::{build_history_context, is_anaphoric, Session, MAX_CHARS},
    tools,
};

#[derive(Clone, Debug)]
struct SchemaCache {
    expires_at: Instant,
    tables: Vec<TableInfo>,
}

pub struct Agent {
    config: Config,
    db: SqlServer,
    llm: Ollama,
    validator: SqlValidator,
    schema: Arc<RwLock<Option<SchemaCache>>>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let policy = SecurityPolicy {
            max_sql_length: config.max_sql_length,
            allowed_tables: config.allowed_tables.clone(),
            block_sensitive_columns: config.block_sensitive_columns,
            block_comments: config.block_comments,
            allow_cte: config.allow_cte,
            allow_system_tables: config.allow_system_tables,
            max_joins: config.max_joins,
            max_subqueries: config.max_subqueries,
        };

        Self {
            db: SqlServer::new(config.clone()),

            llm: Ollama::new(
                config.ollama_url.clone(),
                config.ollama_model.clone(),
                config.ollama_timeout_seconds,
                config.ollama_temperature,
                config.ollama_connect_timeout_seconds,
            ),

            validator: SqlValidator::new(policy),

            schema: Arc::new(RwLock::new(None)),

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
                "question": question
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
            SYSTEM_PROMPT, self.config.database_name
        ));

        let mut messages = vec![system, Message::user(question.to_string())];

        /*
         * ============================================================
         * AGENT LOOP
         * ============================================================
         */

        let mut tool_calls_history: Vec<String> = Vec::new();

        for step in 1..=self.config.max_steps {
            println!("\n━━━━━━━━ STEP {step}/{} ━━━━━━━━", self.config.max_steps);

            let tool_defs = tools::definitions();
            let reply = self
                .llm
                .chat(&messages, &tool_defs, self.config.verbose)
                .await
                .with_context(|| format!("Ollama falló en STEP {step}"))?;

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
                    println!(
                        "⚠️ SQL detectado como texto; \
                         validando y ejecutando..."
                    );

                    let result = self
                        .execute_read_tool(
                            &json!({
                                "sql": text
                            }),
                            &request_id,
                        )
                        .await?;

                    messages.push(reply);

                    messages.push(Message::tool("execute_read_query", result));

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

            println!("🔧 Tool calls: {}", reply.tool_calls.len());

            if reply.tool_calls.len() > self.config.max_tool_calls_per_step {
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
                .take(self.config.max_tool_calls_per_step)
            {
                let name = call.function.name.as_str();

                println!("   ↳ {name}");

                let result = self.dispatch_tool(call, &request_id).await?;

                println!("   ✓ {name} completado");

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

                messages.push(Message::tool(name, result));
            }
        }

        anyhow::bail!("Se alcanzó MAX_STEPS sin obtener una respuesta final")
    }

    /// Build LLM messages with history, schema memory hints and caps.
    /// Pure helper for testing run_with_history without side effects.
    pub fn build_messages_with_history(&self, session: &Session, question: &str) -> Vec<Message> {
        let mut system_content = format!(
            "{} Base de datos: {}.",
            SYSTEM_PROMPT, self.config.database_name
        );
        // Inject valid schema memory hints (TTL filtered)
        let valid_hints: Vec<String> = session
            .schema_memory
            .iter()
            .filter(|(_, e)| !e.is_expired())
            .map(|(k, e)| {
                format!(
                    "{} -> {}.{} (synonyms: {})",
                    k,
                    e.table.schema,
                    e.table.table,
                    e.synonyms.join(", ")
                )
            })
            .collect();
        if !valid_hints.is_empty() {
            system_content.push_str("\n\nMemoria de esquema reciente:\n");
            system_content.push_str(&valid_hints.join("\n"));
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
                "question": question,
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

        for step in 1..=self.config.max_steps {
            if self.config.verbose {
                println!("\n━━━━━━━━ STEP {step}/{} ━━━━━━━━", self.config.max_steps);
            }
            let tool_defs = tools::definitions();
            // messages already includes system + history + question; for LLM call we use the built messages clone
            // But we need to keep messages mutable for loop: we already have messages built, but we need to update it each iteration
            let reply = self
                .llm
                .chat(&messages, &tool_defs, self.config.verbose)
                .await
                .with_context(|| format!("Ollama falló en STEP {step}"))?;

            if reply.tool_calls.is_empty() {
                let text = reply.content.trim();
                if looks_like_sql(text) {
                    let result = self
                        .execute_read_tool(&json!({ "sql": text }), &request_id)
                        .await?;
                    let assistant_msg = reply.clone();
                    session.push(assistant_msg.clone());
                    messages.push(assistant_msg);
                    let tool_msg = Message::tool("execute_read_query", result.clone());
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

            if reply.tool_calls.len() > self.config.max_tool_calls_per_step {
                anyhow::bail!("Demasiadas herramientas en un mismo paso");
            }

            // Must keep assistant tool_calls message before results
            session.push(reply.clone());
            messages.push(reply.clone());

            for call in reply
                .tool_calls
                .iter()
                .take(self.config.max_tool_calls_per_step)
            {
                let name = call.function.name.as_str();
                if self.config.verbose {
                    println!("   ↳ {name}");
                }
                // For schema-related tools, update schema_memory before/after
                let result = self
                    .dispatch_tool_with_history(call, &request_id, session)
                    .await?;
                if self.config.verbose {
                    println!("   ✓ {name} completado");
                    println!("📦 TOOL RESULT [{name}]:\n{result}");
                }
                tool_calls_history.push(name.to_string());
                let tool_msg = Message::tool(name, result);
                session.push(tool_msg.clone());
                messages.push(tool_msg);
            }
            let _ = session.persist().await;
        }

        anyhow::bail!("Se alcanzó MAX_STEPS sin obtener una respuesta final")
    }

    async fn dispatch_tool_with_history(
        &self,
        call: &ToolCall,
        request_id: &str,
        session: &mut Session,
    ) -> Result<String> {
        let args = normalize_tool_arguments(&call.function.arguments)?;
        match call.function.name.as_str() {
            "search_schema" => {
                let query_raw = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let result = self.search_schema(&args).await?;
                // Update schema memory: parse matched tables from result? For now, try to extract from cached tables
                // We use search_with_fallback to get matched tables and upsert
                if let Ok(tables) = self.cached_tables().await {
                    let allowed: Vec<TableInfo> = tables
                        .iter()
                        .filter(|t| self.table_allowed(&t.schema, &t.table))
                        .cloned()
                        .collect();
                    let (matched, _) =
                        search_with_fallback(&query_raw, &allowed, self.config.max_schema_results);
                    for tbl in &matched {
                        let key = format!("{}.{}", tbl.schema, tbl.table);
                        // Try to get columns for memory entry (best effort)
                        let cols = self.db.describe_table(&key).await.unwrap_or_default();
                        upsert_schema_memory(
                            &mut session.schema_memory,
                            key,
                            tbl.clone(),
                            cols,
                            Some(query_raw.clone()),
                            self.config.schema_cache_seconds,
                        );
                    }
                }
                Ok(result)
            }
            "describe_table" => {
                let table = args
                    .get("table")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let result = self.describe_table_tool(&args).await?;
                // Upsert schema memory for described table
                let (schema, name) = split_table(&table);
                let key = format!("{}.{}", schema, name);
                // Try to get columns to store
                if let Ok(cols) = self.db.describe_table(&key).await {
                    let tbl = TableInfo {
                        schema: schema.clone(),
                        table: name.clone(),
                        // Real type travels on discovery lists; unknown on this path.
                        table_type: String::new(),
                    };
                    upsert_schema_memory(
                        &mut session.schema_memory,
                        key,
                        tbl,
                        cols,
                        Some(table.clone()),
                        self.config.schema_cache_seconds,
                    );
                }
                Ok(result)
            }
            "list_tables" => self.list_tables_tool().await,
            "search_columns" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let matches = self.search_columns_data(&query).await?;
                // Ground memory on the matched tables (bounded, best effort).
                for m in matches.iter().take(5) {
                    let key = format!("{}.{}", m.schema, m.table);
                    if let Ok(cols) = self.db.describe_table(&key).await {
                        upsert_schema_memory(
                            &mut session.schema_memory,
                            key,
                            TableInfo {
                                schema: m.schema.clone(),
                                table: m.table.clone(),
                                table_type: String::new(),
                            },
                            cols,
                            Some(query.clone()),
                            self.config.schema_cache_seconds,
                        );
                    }
                }
                Ok(limit_text(
                    &format_column_matches(&matches, &query),
                    self.config.max_tool_result_chars,
                ))
            }
            "execute_read_query" => self.execute_read_tool(&args, request_id).await,
            other => anyhow::bail!("Tool no permitida: {other}"),
        }
    }

    /*
     * ================================================================
     * VALIDADOR DE COMPLETITUD
     * ================================================================
     */

    /*
     * ================================================================
     * TOOL DISPATCH
     * ================================================================
     */

    async fn dispatch_tool(&self, call: &ToolCall, request_id: &str) -> Result<String> {
        let args = normalize_tool_arguments(&call.function.arguments)?;

        match call.function.name.as_str() {
            "search_schema" => self.search_schema(&args).await,

            "describe_table" => self.describe_table_tool(&args).await,

            "list_tables" => self.list_tables_tool().await,

            "search_columns" => self.search_columns_tool(&args).await,

            "execute_read_query" => self.execute_read_tool(&args, request_id).await,

            other => {
                anyhow::bail!("Tool no permitida: {other}");
            }
        }
    }

    /*
     * ================================================================
     * SEARCH SCHEMA
     * ================================================================
     */

    async fn search_schema(&self, args: &Value) -> Result<String> {
        let query_raw = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        let tables = self.cached_tables().await?;

        let allowed: Vec<TableInfo> = tables
            .iter()
            .filter(|t| self.table_allowed(&t.schema, &t.table))
            .cloned()
            .collect();

        let (matched, did_you_mean) =
            search_with_fallback(&query_raw, &allowed, self.config.max_schema_results);

        println!("🔎 search_schema → {} coincidencias", matched.len());

        let mut output = String::new();

        output.push_str(&format!("✓ TABLAS ENCONTRADAS: {}\n", matched.len()));

        if matched.is_empty() {
            output.push_str("No se encontraron tablas.\n");
            if let Some(suggestion) = did_you_mean {
                // Top-K suggestions ranked by Levenshtein for the empty-result case
                let norm_query = query_raw
                    .split_whitespace()
                    .map(normalize_term)
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut ranked_all: Vec<(TableInfo, usize)> = allowed
                    .iter()
                    .map(|t| {
                        let nt = normalize_term(&t.table);
                        let score = if norm_query.is_empty() {
                            0
                        } else {
                            levenshtein(&norm_query, &nt)
                        };
                        (t.clone(), score)
                    })
                    .collect();
                ranked_all.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.table.cmp(&b.0.table)));
                let top: Vec<TableInfo> = ranked_all
                    .into_iter()
                    .take(self.config.max_schema_results)
                    .map(|(t, _)| t)
                    .collect();
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
            } else if allowed.is_empty() {
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

        Ok(limit_text(&output, self.config.max_tool_result_chars))
    }

    /*
     * ================================================================
     * DESCRIBE TABLE
     * ================================================================
     */

    async fn describe_table_tool(&self, args: &Value) -> Result<String> {
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

        println!("📐 Describiendo {}.{}", schema, name);

        let detail: TableDetail = self
            .db
            .describe_table_full(&format!("{}.{}", schema, name))
            .await?;

        Ok(limit_text(
            &format_table_detail(&schema, &name, &detail),
            self.config.max_tool_result_chars,
        ))
    }

    /*
     * ================================================================
     * DISCOVERY TOOLS (SG-1)
     * ================================================================
     */

    /// List every visible table and view (SG-1). Cached via `cached_tables`.
    async fn list_tables_tool(&self) -> Result<String> {
        let tables = self.cached_tables().await?;
        let allowed: Vec<TableInfo> = tables
            .iter()
            .filter(|t| self.table_allowed(&t.schema, &t.table))
            .cloned()
            .collect();
        Ok(limit_text(
            &format_table_list(&allowed),
            self.config.max_tool_result_chars,
        ))
    }

    /// Structured column search filtered by the allowlist (SG-1).
    async fn search_columns_data(&self, query: &str) -> Result<Vec<ColumnMatch>> {
        if query.trim().is_empty() {
            anyhow::bail!("Falta query");
        }
        let matches = self.db.search_columns(query).await?;
        Ok(matches
            .into_iter()
            .filter(|m| self.table_allowed(&m.schema, &m.table))
            .collect())
    }

    /// Formatted column search for the LLM (SG-1).
    async fn search_columns_tool(&self, args: &Value) -> Result<String> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let matches = self.search_columns_data(&query).await?;
        Ok(limit_text(
            &format_column_matches(&matches, &query),
            self.config.max_tool_result_chars,
        ))
    }

    /*
     * ================================================================
     * EXECUTE READ QUERY
     * ================================================================
     */

    async fn execute_read_tool(&self, args: &Value, request_id: &str) -> Result<String> {
        let sql = args.get("sql").and_then(Value::as_str).unwrap_or("").trim();

        if sql.is_empty() {
            anyhow::bail!("Falta sql");
        }

        println!("🔐 Validando SQL...");

        self.validator
            .validate(sql)
            .context("SQL bloqueado por política de seguridad")?;

        println!("✅ SQL válido");

        self.audit(
            "sql_approved",
            json!({
                "request_id": request_id,
                "sql": if self.config.audit_sql {
                    json!(sql)
                } else {
                    json!("[REDACTED]")
                }
            }),
        )
        .await?;

        println!("🗄️ Ejecutando consulta...");

        let result = match self.db.execute_read(sql).await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if msg.to_ascii_lowercase().contains("invalid object name") {
                    // Re-inject candidates (top 17) for self-correction within MAX_STEPS
                    let tables = self.cached_tables().await.unwrap_or_default();
                    let allowed: Vec<TableInfo> = tables
                        .iter()
                        .filter(|t| self.table_allowed(&t.schema, &t.table))
                        .cloned()
                        .collect();
                    let out = format_invalid_reinject(&allowed, &msg);
                    tracing::warn!(
                        "Invalid object name re-injected {} candidates for sql: {}",
                        allowed.len().min(17),
                        sql
                    );
                    return Ok(limit_text(&out, self.config.max_tool_result_chars));
                } else {
                    return Err(e);
                }
            }
        };

        println!(
            "📊 {} fila(s){}",
            result.row_count,
            if result.truncated { " [LIMITADO]" } else { "" }
        );

        /*
         * Formatear resultados de forma clara para LLM pequeño
         */
        let mut formatted = format!("✓ RESULTADOS ({} filas)\n\n", result.row_count);

        if result.rows.is_empty() {
            formatted.push_str("No se encontraron datos.\n");
        } else {
            /*
             * Mostrar cada fila de forma clara
             */
            for (idx, row) in result.rows.iter().enumerate() {
                formatted.push_str(&format!("Fila {}:\n", idx + 1));

                if let Some(obj) = row.as_object() {
                    for (key, val) in obj {
                        let display_val = match val {
                            Value::Null => "[NULL]".to_string(),
                            Value::Number(n) => n.to_string(),
                            Value::String(s) => s.clone(),
                            Value::Bool(b) => b.to_string(),
                            _ => "[complex]".to_string(),
                        };

                        formatted.push_str(&format!("  {} = {}\n", key, display_val));
                    }
                } else {
                    formatted.push_str(&format!("  {}\n", row));
                }

                formatted.push('\n');
            }
        }

        if result.truncated {
            formatted.push_str("\n[Nota: Resultado limitado al máximo configurado]\n");
        }

        Ok(limit_text(&formatted, self.config.max_tool_result_chars))
    }

    /*
     * ================================================================
     * AUDIT
     * ================================================================
     */

    async fn audit(&self, event: &str, payload: Value) -> Result<()> {
        if self.config.audit_enabled {
            audit::write(&self.config.audit_path, event, payload).await?;
        }

        Ok(())
    }

    /*
     * ================================================================
     * SCHEMA CACHE
     * ================================================================
     */

    async fn cached_tables(&self) -> Result<Vec<TableInfo>> {
        {
            let guard = self.schema.read().await;

            if let Some(cache) = &*guard {
                if cache.expires_at > Instant::now() {
                    println!("⚡ Esquema desde caché");

                    return Ok(cache.tables.clone());
                }
            }
        }

        println!(
            "🗄️ SQL Server → \
             INFORMATION_SCHEMA.TABLES..."
        );

        let tables = self.db.list_tables().await?;

        println!("🔎 search_schema: {} tablas encontradas", tables.len());

        // Validate allowlist vs live (drift detection)
        if !self.config.allowed_tables.is_empty() {
            let live_names: Vec<String> = tables
                .iter()
                .map(|t| format!("{}.{}", t.schema, t.table))
                .collect();
            let drifted = Config::find_drifted(&self.config.allowed_tables, &live_names);
            if !drifted.is_empty() {
                tracing::warn!(
                    drifted = ?drifted,
                    "Allowlist drift: allowed tables not found in live DB"
                );
                println!(
                    "⚠️ Allowlist drift: no encontradas en BD: {}",
                    drifted.join(", ")
                );
            }
        }

        /*
         * Guardamos copia del esquema.
         */

        *self.schema.write().await = Some(SchemaCache {
            expires_at: Instant::now() + Duration::from_secs(self.config.schema_cache_seconds),
            tables: tables.clone(),
        });

        Ok(tables)
    }

    /*
     * ================================================================
     * TABLE ALLOWLIST
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
            for t in allowed.iter().take(self.config.max_schema_results) {
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

    fn table_allowed(&self, schema: &str, table: &str) -> bool {
        /*
         * Nunca permitir esquemas del sistema
         * si la política está desactivada.
         */

        if !self.config.allow_system_tables
            && (schema.eq_ignore_ascii_case("sys")
                || schema.eq_ignore_ascii_case("information_schema"))
        {
            return false;
        }

        /*
         * Si no existe allowlist,
         * permitimos las tablas visibles
         * excepto las de sistema.
         */

        if self.config.allowed_tables.is_empty() {
            return true;
        }

        let full = format!("{}.{}", schema, table).to_ascii_lowercase();

        self.config
            .allowed_tables
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&full) || x.eq_ignore_ascii_case(table))
    }
}

/*
 * ====================================================================
 * TOOL ARGUMENTS
 * ====================================================================
 */

fn normalize_tool_arguments(v: &Value) -> Result<Value> {
    match v {
        Value::Object(_) => Ok(v.clone()),

        Value::String(s) => {
            serde_json::from_str(s).context("Argumentos de tool no son JSON válido")
        }

        _ => {
            anyhow::bail!(
                "Argumentos de tool deben ser \
                 un objeto JSON"
            );
        }
    }
}

/*
 * ====================================================================
 * TABLE NAME
 * ====================================================================
 */

fn split_table(s: &str) -> (String, String) {
    let clean = s.replace(['[', ']', '"'], "");

    let parts: Vec<&str> = clean.split('.').collect();

    if parts.len() >= 2 {
        (
            parts[parts.len() - 2].to_string(),
            parts[parts.len() - 1].to_string(),
        )
    } else {
        tracing::warn!(
            "split_table: no schema supplied for '{}', defaulting to dbo (explicit schema recommended)",
            s
        );
        ("dbo".into(), clean)
    }
}

#[cfg(test)]
pub fn split_table_pub(s: &str) -> (String, String) {
    split_table(s)
}

/*
 * ====================================================================
 * LIMIT TEXT
 * ====================================================================
 */

fn limit_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }

    let mut end = max.min(text.len());

    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...[resultado recortado]", &text[..end])
}

/*
 * ====================================================================
 * SQL DETECTION
 * ====================================================================
 */

fn looks_like_sql(s: &str) -> bool {
    let t = s.trim_start().to_ascii_uppercase();

    t.starts_with("SELECT ") || t == "SELECT" || t.starts_with("WITH ") || t == "WITH"
}

/*
 * ====================================================================
 * SEARCH HARDENING HELPERS (P0/P1 grounding)
 * ====================================================================
 */

pub fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            _ => c,
        })
        .collect()
}

pub fn singularize(s: &str) -> String {
    if s.len() > 3 && s.ends_with("es") {
        s[..s.len() - 2].to_string()
    } else if s.len() > 2 && s.ends_with('s') {
        s[..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = std::cmp::min(
                std::cmp::min(prev[j] + 1, curr[j - 1] + 1),
                prev[j - 1] + cost,
            );
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

pub fn normalize_term(s: &str) -> String {
    let lower = s.to_lowercase();
    let stripped = strip_accents(&lower);
    singularize(&stripped)
}

/// Format the full table/view listing for the LLM (SG-1 discovery).
/// Pure helper: every entry shows schema, name and type (BASE TABLE / VIEW).
pub fn format_table_list(tables: &[TableInfo]) -> String {
    let mut out = format!("✓ TABLAS Y VISTAS: {}\n", tables.len());
    if tables.is_empty() {
        out.push_str(
            "No hay tablas visibles para tu filtro.\n\
             ⚠️ No inventes nombres de tabla; pide aclaración o ajusta el filtro.\n",
        );
    } else {
        for t in tables {
            out.push_str(&format!("  • {}.{} [{}]\n", t.schema, t.table, t.table_type));
        }
        out.push_str(
            "\n→ SIGUIENTE PASO: usa describe_table con el nombre calificado \
             EXACTO de la lista, o search_columns para buscar por columna.\n",
        );
    }
    out
}

/// Format column search results for the LLM (SG-1 discovery).
/// Pure helper: every match shows its qualified table.column pair.
pub fn format_column_matches(matches: &[ColumnMatch], query: &str) -> String {
    let mut out = format!("✓ COLUMNAS para '{query}': {}\n", matches.len());
    if matches.is_empty() {
        out.push_str(&format!(
            "No se encontraron columnas para '{query}'.\n\
             → Usa list_tables para explorar el esquema completo; \
             no inventes nombres de columna.\n"
        ));
    } else {
        for m in matches {
            out.push_str(&format!(
                "  • {}.{}.{} ({})\n",
                m.schema, m.table, m.column, m.data_type
            ));
        }
        out.push_str(
            "\n→ SIGUIENTE PASO: usa describe_table con la tabla EXACTA \
             de la lista y luego execute_read_query.\n",
        );
    }
    out
}

/// Format the enriched describe output for the LLM (SG-2).
/// Pure helper: ESTRUCTURA + PK + FK + [VIEW definition] + MUESTRA + COUNT(*).
pub fn format_table_detail(schema: &str, name: &str, detail: &TableDetail) -> String {
    let mut out = format!("\n✓ ESTRUCTURA DE {schema}.{name}\n\nCOLUMNAS:\n");
    for col in &detail.columns {
        out.push_str(&format!(
            "  • {} ({}){}\n",
            col.column,
            col.data_type,
            if col.nullable {
                " [NULLABLE]"
            } else {
                " [NO NULO]"
            }
        ));
    }

    if detail.primary_keys.is_empty() {
        out.push_str("\nPRIMARY KEY: (ninguna)\n");
    } else {
        out.push_str(&format!(
            "\nPRIMARY KEY: {}\n",
            detail.primary_keys.join(", ")
        ));
    }

    if detail.foreign_keys.is_empty() {
        out.push_str("FOREIGN KEYS: (ninguna)\n");
    } else {
        out.push_str("FOREIGN KEYS:\n");
        for fk in &detail.foreign_keys {
            out.push_str(&format!(
                "  • {} → {}.{}({})\n",
                fk.column, fk.ref_schema, fk.ref_table, fk.ref_column
            ));
        }
    }

    if let Some(def) = &detail.view_definition {
        out.push_str(&format!("\n[VIEW] Definición:\n{def}\n"));
    } else {
        // None is ambiguous: base table OR view hidden by a missing VIEW
        // DEFINITION grant. Never stay silent so the LLM does not misread
        // absence as "not a view".
        out.push_str("\n[VIEW] Definición: definition unavailable (permissions) — base table or missing VIEW DEFINITION grant.\n");
    }

    out.push_str(&format!(
        "\nMUESTRA (TOP 5, {} filas):\n",
        detail.sample_rows.len()
    ));
    if detail.sample_rows.is_empty() {
        out.push_str("  (sin filas)\n");
    } else {
        for (idx, row) in detail.sample_rows.iter().enumerate() {
            if let Some(obj) = row.as_object() {
                let cells: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| {
                        let display = match v {
                            Value::Null => "[NULL]".to_string(),
                            Value::Number(n) => n.to_string(),
                            Value::String(s) => s.clone(),
                            Value::Bool(b) => b.to_string(),
                            _ => "[complex]".to_string(),
                        };
                        format!("{k} = {display}")
                    })
                    .collect();
                out.push_str(&format!("  Fila {}: {}\n", idx + 1, cells.join(", ")));
            } else {
                out.push_str(&format!("  Fila {}: {row}\n", idx + 1));
            }
        }
    }

    // Row count -1 means unknown (bounded COUNT timed out or failed);
    // structure/sample above are still complete, so report unknown explicitly.
    if detail.row_count < 0 {
        out.push_str("\nCOUNT(*): unknown (COUNT capped/timed out — structure above is complete)\n");
    } else {
        out.push_str(&format!("\nCOUNT(*): {}\n", detail.row_count));
    }
    out.push_str(
        "\n→ IMPORTANTE: Esto incluye ESTRUCTURA y MUESTRA. \
         Para más DATOS usa execute_read_query con un SELECT.",
    );
    out
}

pub fn format_invalid_reinject(candidates: &[TableInfo], error_msg: &str) -> String {
    let mut out = format!("❌ Invalid object name: {}\n", error_msg);
    out.push_str("→ La tabla no existe. Usa solo nombres calificados de search_schema.\n");
    out.push_str("Candidatos disponibles (17 máx):\n");
    for t in candidates.iter().take(17) {
        out.push_str(&format!("  • {}.{}\n", t.schema, t.table));
    }
    if candidates.is_empty() {
        out.push_str("  (no hay candidatos visibles)\n");
    }
    out.push_str(
        "→ Corrige el SQL usando un nombre de la lista y reintenta dentro de MAX_STEPS.\n",
    );
    out
}

/// Pure ranking helper used by `search_schema` and tests.
/// Returns ranked matches after AND→OR fallback, truncated to `max_results`.
/// For empty query, returns first `max_results` tables (no ranking).
#[allow(dead_code)]
pub fn filter_and_rank_tables(
    query: &str,
    tables: &[TableInfo],
    max_results: usize,
) -> Vec<TableInfo> {
    let (ranked, _) = search_with_fallback(query, tables, max_results);
    ranked
}

/// Returns (ranked_matches, did_you_mean) where `did_you_mean` is Some(closest)
/// when no matches were found (for Did-you-mean suggestion). When query is empty,
/// `did_you_mean` is None and `ranked` is truncated list.
pub fn search_with_fallback(
    query: &str,
    tables: &[TableInfo],
    max_results: usize,
) -> (Vec<TableInfo>, Option<TableInfo>) {
    let q = query.trim();
    if q.is_empty() {
        let mut all = tables.to_vec();
        all.truncate(max_results);
        return (all, None);
    }
    let terms: Vec<String> = q.split_whitespace().map(normalize_term).collect();
    let norm_query_joined = terms.join(" ");

    // Helper to compute normalized full/table for a TableInfo
    let norm_for = |t: &TableInfo| -> (String, String) {
        let full = format!("{}.{}", t.schema, t.table);
        let nf = normalize_term(&full);
        let nt = normalize_term(&t.table);
        (nf, nt)
    };

    // AND phase
    let mut and_matches: Vec<(TableInfo, usize)> = Vec::new();
    for t in tables {
        let (nf, nt) = norm_for(t);
        let all_contain = terms
            .iter()
            .all(|term| nf.contains(term) || nt.contains(term));
        if all_contain {
            // rank score: Levenshtein between normalized query joined and nt (or nf)
            // Use minimum distance among terms vs table for multi-term
            let score = if terms.len() == 1 {
                levenshtein(&norm_query_joined, &nt)
            } else {
                // For multi-term, use sum of min distances per term? Simpler: min distance
                terms
                    .iter()
                    .map(|term| levenshtein(term, &nt))
                    .min()
                    .unwrap_or(usize::MAX)
            };
            and_matches.push((t.clone(), score));
        }
    }
    if !and_matches.is_empty() {
        and_matches.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.table.cmp(&b.0.table)));
        let mut out: Vec<TableInfo> = and_matches.into_iter().map(|(t, _)| t).collect();
        out.truncate(max_results);
        return (out, None);
    }

    // OR phase
    let mut or_matches: Vec<(TableInfo, usize)> = Vec::new();
    for t in tables {
        let (nf, nt) = norm_for(t);
        let any_contain = terms
            .iter()
            .any(|term| nf.contains(term) || nt.contains(term));
        if any_contain {
            let score = if terms.len() == 1 {
                levenshtein(&norm_query_joined, &nt)
            } else {
                terms
                    .iter()
                    .map(|term| levenshtein(term, &nt))
                    .min()
                    .unwrap_or(usize::MAX)
            };
            or_matches.push((t.clone(), score));
        }
    }
    if !or_matches.is_empty() {
        or_matches.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.table.cmp(&b.0.table)));
        let mut out: Vec<TableInfo> = or_matches.into_iter().map(|(t, _)| t).collect();
        out.truncate(max_results);
        return (out, None);
    }

    // Zero matches → Did-you-mean: rank all by Levenshtein and suggest closest
    let mut all_ranked: Vec<(TableInfo, usize)> = tables
        .iter()
        .map(|t| {
            let (_, nt) = norm_for(t);
            let score = levenshtein(&norm_query_joined, &nt);
            (t.clone(), score)
        })
        .collect();
    all_ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.table.cmp(&b.0.table)));
    let suggestion = all_ranked.first().map(|(t, _)| t.clone());
    // Return empty ranked but with suggestion; caller will format 0 + Did-you-mean
    (Vec::new(), suggestion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::TableInfo;

    fn make_tables(names: &[(&str, &str)]) -> Vec<TableInfo> {
        names
            .iter()
            .map(|(s, t)| TableInfo {
                schema: s.to_string(),
                table: t.to_string(),
                table_type: "BASE TABLE".to_string(),
            })
            .collect()
    }

    #[test]
    fn singularize_usuarios() {
        assert_eq!(singularize("usuarios"), "usuario");
    }

    #[test]
    fn singularize_roles() {
        assert_eq!(singularize("roles"), "rol");
    }

    #[test]
    fn singularize_meses() {
        // "meses" -> "mes" via es removal
        assert_eq!(singularize("meses"), "mes");
    }

    #[test]
    fn strip_accents_cancion() {
        assert_eq!(strip_accents("canción"), "cancion");
    }

    #[test]
    fn strip_accents_espanol() {
        assert_eq!(strip_accents("español"), "espanol");
    }

    #[test]
    fn levenshtein_kitten_sitting() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn levenshtein_usuarios_usuario() {
        // "usuarios" vs "usuario" distance 1 (extra s)
        assert_eq!(levenshtein("usuarios", "usuario"), 1);
    }

    #[test]
    fn normalize_term_usuarios() {
        assert_eq!(normalize_term("Usuarios"), "usuario");
    }

    #[test]
    fn normalize_term_with_accent() {
        assert_eq!(normalize_term("Canciones"), "cancion");
    }

    #[test]
    fn search_rank_usuarios_finds_usuario_first() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto"), ("dbo", "Pedido")]);
        let ranked = filter_and_rank_tables("usuarios", &tables, 20);
        assert!(!ranked.is_empty(), "should find at least one");
        assert_eq!(ranked[0].table, "Usuario");
    }

    #[test]
    fn search_or_fallback_finds_partial() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        // query "usu prod" AND would require both terms in same table -> 0, OR should find both
        let ranked = filter_and_rank_tables("usu prod", &tables, 20);
        // With OR fallback, should find both tables (each matches one term)
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn search_zero_returns_did_you_mean_candidates() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        let (ranked, did_you_mean) = search_with_fallback("xyz_noexiste", &tables, 20);
        assert!(ranked.is_empty(), "no direct match");
        assert!(did_you_mean.is_some(), "should suggest Did you mean");
        let dm = did_you_mean.unwrap();
        assert!(
            dm.table == "Usuario" || dm.table == "Producto",
            "Did you mean should be one of the candidates, got {}",
            dm.table
        );
    }

    #[test]
    fn search_truncates_to_max_k() {
        let mut names = Vec::new();
        for i in 0..30 {
            names.push((format!("dbo"), format!("Tabla{i:02}")));
        }
        // Convert to &[(&str,&str)] not easy, so build tables directly
        let tables: Vec<TableInfo> = (0..30)
            .map(|i| TableInfo {
                schema: "dbo".into(),
                table: format!("Tabla{i:02}"),
                table_type: "BASE TABLE".into(),
            })
            .collect();
        let ranked = filter_and_rank_tables("", &tables, 20);
        assert_eq!(ranked.len(), 20, "should truncate to MAX_SCHEMA_RESULTS 20");
    }

    #[test]
    fn split_table_defaults_to_dbo() {
        let (schema, table) = split_table_pub("usuarios");
        assert_eq!(schema, "dbo");
        assert_eq!(table, "usuarios");
    }

    #[test]
    fn split_table_with_schema_keeps_schema() {
        let (s, t) = split_table_pub("dbo.Usuario");
        assert_eq!(s, "dbo");
        assert_eq!(t, "Usuario");
        let (s2, t2) = split_table_pub("[dbo].[Usuario]");
        assert_eq!(s2, "dbo");
        assert_eq!(t2, "Usuario");
    }

    #[test]
    fn format_invalid_reinject_contains_candidates() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        let msg = format_invalid_reinject(&tables, "Invalid object name 'dbo.usuarios'");
        assert!(msg.contains("Invalid object name"));
        assert!(msg.contains("dbo.Usuario"));
        assert!(msg.contains("dbo.Producto"));
        assert!(msg.contains("17 máx") || msg.contains("Candidatos"));
    }

    #[test]
    fn format_invalid_reinject_limits_to_17() {
        let tables: Vec<TableInfo> = (0..30)
            .map(|i| TableInfo {
                schema: "dbo".into(),
                table: format!("Tabla{i:02}"),
                table_type: "BASE TABLE".into(),
            })
            .collect();
        let msg = format_invalid_reinject(&tables, "Invalid object name 'dbo.foo'");
        // Should contain only first 17
        assert!(msg.contains("Tabla00"));
        assert!(msg.contains("Tabla16"));
        assert!(!msg.contains("Tabla17"), "should limit to 17 candidates");
    }

    // ===== Task 2.4 discovery formatters (SG-1/SG-2) =====
    use crate::database::schema::{ColumnMatch, ForeignKeyInfo, TableDetail};

    fn sample_tables_with_types() -> Vec<TableInfo> {
        vec![
            TableInfo {
                schema: "dbo".into(),
                table: "Orders".into(),
                table_type: "BASE TABLE".into(),
            },
            TableInfo {
                schema: "dbo".into(),
                table: "VwActive".into(),
                table_type: "VIEW".into(),
            },
        ]
    }

    #[test]
    fn format_table_list_shows_schema_and_type() {
        let out = format_table_list(&sample_tables_with_types());
        assert!(out.contains("dbo.Orders"), "got: {out}");
        assert!(out.contains("BASE TABLE"), "got: {out}");
        assert!(out.contains("dbo.VwActive"), "got: {out}");
        assert!(out.contains("VIEW"), "got: {out}");
    }

    #[test]
    fn format_table_list_empty_warns_without_guessing() {
        let out = format_table_list(&[]);
        assert!(out.contains('0'), "got: {out}");
        assert!(
            out.to_lowercase().contains("no inventes") || out.to_lowercase().contains("no hay"),
            "empty list must warn against inventing names, got: {out}"
        );
    }

    #[test]
    fn format_column_matches_returns_table_column_pairs() {
        let matches = vec![ColumnMatch {
            schema: "dbo".into(),
            table: "Orders".into(),
            column: "email".into(),
            data_type: "nvarchar".into(),
        }];
        let out = format_column_matches(&matches, "email");
        assert!(out.contains("dbo.Orders"), "got: {out}");
        assert!(out.contains("email"), "got: {out}");
    }

    #[test]
    fn format_column_matches_empty_suggests_discovery() {
        let out = format_column_matches(&[], "zzz_noexiste");
        assert!(out.contains("zzz_noexiste"), "got: {out}");
        assert!(
            out.contains("list_tables"),
            "no-match must point back to discovery, got: {out}"
        );
    }

    fn sample_detail() -> (String, String, TableDetail) {
        use serde_json::json;
        (
            "dbo".into(),
            "Orders".into(),
            TableDetail {
                columns: vec![crate::database::ColumnInfo {
                    column: "id".into(),
                    data_type: "int".into(),
                    nullable: false,
                    ordinal: 1,
                }],
                primary_keys: vec!["id".into()],
                foreign_keys: vec![ForeignKeyInfo {
                    column: "user_id".into(),
                    ref_schema: "dbo".into(),
                    ref_table: "Usuario".into(),
                    ref_column: "id".into(),
                }],
                view_definition: None,
                sample_rows: vec![json!({"id": 1})],
                row_count: 42,
            },
        )
    }

    #[test]
    fn format_table_detail_full_table_sections() {
        let (s, t, d) = sample_detail();
        let out = format_table_detail(&s, &t, &d);
        assert!(out.contains("ESTRUCTURA"), "got: {out}");
        assert!(out.contains("PRIMARY KEY") || out.contains("id"), "got: {out}");
        assert!(out.contains("dbo.Usuario"), "FK target must appear, got: {out}");
        assert!(out.contains("42"), "COUNT(*) must appear, got: {out}");
        assert!(out.contains("MUESTRA"), "got: {out}");
    }

    #[test]
    fn format_table_detail_view_and_empty() {
        let d = TableDetail {
            columns: vec![],
            primary_keys: vec![],
            foreign_keys: vec![],
            view_definition: Some("SELECT id FROM dbo.Orders".into()),
            sample_rows: vec![],
            row_count: 0,
        };
        let out = format_table_detail("dbo", "VwEmpty", &d);
        assert!(out.contains("VIEW"), "view marker must appear, got: {out}");
        assert!(
            out.contains("SELECT id FROM dbo.Orders"),
            "view definition must appear, got: {out}"
        );
        assert!(out.contains('0'), "zero count must appear, got: {out}");
    }

    #[test]
    fn format_table_detail_none_definition_shows_permissions_hint() {
        // Views without VIEW DEFINITION grant arrive with None; the formatter
        // must hint instead of staying silent.
        let d = TableDetail {
            columns: vec![],
            primary_keys: vec![],
            foreign_keys: vec![],
            view_definition: None,
            sample_rows: vec![],
            row_count: 10,
        };
        let out = format_table_detail("dbo", "VwHidden", &d);
        assert!(
            out.contains("definition unavailable (permissions)"),
            "missing definition must hint permissions, got: {out}"
        );
    }

    #[test]
    fn format_table_detail_unknown_count_still_shows_structure() {
        // Bounded COUNT timeout yields row_count -1; structure must survive.
        let (s, t, mut d) = sample_detail();
        d.row_count = -1;
        let out = format_table_detail(&s, &t, &d);
        assert!(out.contains("ESTRUCTURA"), "structure must survive, got: {out}");
        assert!(out.contains("MUESTRA"), "sample section must survive, got: {out}");
        assert!(
            out.contains("unknown"),
            "unknown count must be explicit, got: {out}"
        );
    }

    #[tokio::test]
    async fn search_columns_data_rejects_empty_query_without_db() {
        let agent = Agent::new(dummy_config());
        assert!(agent.search_columns_data("   ").await.is_err());
    }

    // ===== Task 2.4 run_with_history helpers =====
    fn dummy_config() -> crate::config::Config {
        crate::config::Config {
            database_host: "localhost".into(),
            database_port: 1433,
            database_name: "TestDB".into(),
            database_user: "user".into(),
            database_password: "pass".into(),
            database_trust_cert: true,
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "qwen3:4b".into(),
            ollama_timeout_seconds: 120,
            ollama_connect_timeout_seconds: 5,
            ollama_temperature: 0.0,
            max_steps: 8,
            max_sql_length: 10_000,
            max_rows: 100,
            max_result_chars: 30_000,
            schema_cache_seconds: 300,
            query_timeout_seconds: 30,
            max_concurrent_queries: 1,
            max_joins: 5,
            max_subqueries: 5,
            max_schema_results: 20,
            max_tool_result_chars: 20_000,
            max_tool_calls_per_step: 10,
            allowed_tables: vec![],
            block_sensitive_columns: true,
            block_comments: true,
            allow_cte: true,
            allow_system_tables: false,
            audit_enabled: false,
            audit_path: "logs/test-audit.jsonl".into(),
            audit_sql: false,
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
                tables: vec![],
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
}
