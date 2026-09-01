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
    database::{ColumnInfo, SqlServer, TableInfo},
    llm::{Message, Ollama, ToolCall},
    security::{SecurityPolicy, SqlValidator},
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
         * SYSTEM PROMPT
         * ============================================================
         */

        let system = Message::system(format!(
            r#"Eres un agente SQL Server. Base de datos: {}.

OBJETIVO: Responder preguntas con DATOS REALES de la BD.

REGLAS:
✓ Solo SELECT y WITH...SELECT
✗ NO: INSERT, UPDATE, DELETE, DROP, ALTER, EXEC, procedimientos

CUÁNDO USAR CADA HERRAMIENTA:

1. Si pregunta es sobre DATOS (cuántos, cuáles, listar, valores, totales, ventas):
   a) Usa search_schema("palabra_clave") para encontrar tabla
   b) Usa describe_table("dbo.nombre_tabla") para ver columnas
   c) Usa execute_read_query("SELECT ...") para OBTENER DATOS REALES

2. Si pregunta es sobre ESTRUCTURA (columnas, tipos, qué campos tiene):
   a) search_schema + describe_table SOLO
   b) NO uses execute_read_query

EJEMPLOS DE PREGUNTAS Y QUÉ HACER:

PREGUNTA: "Cuántos roles existen y cuáles son"
PASO 1: search_schema("rol") → encuentra dbo.rol
PASO 2: describe_table("dbo.rol") → ve columnas: id, nombre, código, estado
PASO 3: execute_read_query("SELECT id, nombre, código FROM dbo.rol")
RESPUESTA: "Existen N roles. Los roles son: [lista con nombres]"

PREGUNTA: "Ventas totales por mes"
PASO 1: search_schema("venta")
PASO 2: describe_table("dbo.ventas")
PASO 3: execute_read_query("SELECT MONTH(fecha) mes, SUM(monto) total FROM dbo.ventas GROUP BY MONTH(fecha)")
RESPUESTA: "Las ventas por mes son: [datos reales]"

PREGUNTA: "Qué columnas tiene la tabla cliente"
PASO 1: describe_table("dbo.cliente")
PASO 2: NO USES execute_read_query
RESPUESTA: "La tabla cliente tiene columnas: [lista]"

REGLAS CRÍTICAS:
- NUNCA escribas SQL sin ejecutarlo con execute_read_query
- NUNCA inventes datos o resultados
- Si necesitas datos → DEBES ejecutar execute_read_query
- Responde SOLO en español
- Muestra números reales: "Hay 7 roles" NO "aproximadamente muchos roles"

ACCIÓN:
1. Lee la pregunta
2. Ejecuta las herramientas necesarias (sin dudar)
3. Responde con datos reales o estructura real

Confía en que sabes SQL. Usa SUM, COUNT, GROUP BY, WHERE, JOINs, DATEADD sin miedo.
Eres experto en SQL Server."#,
            self.config.database_name
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

            let reply = self
                .llm
                .chat(&messages, self.config.verbose)
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
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        let tables = self.cached_tables().await?;

        let mut matches: Vec<&TableInfo> = tables
            .iter()
            .filter(|t| self.table_allowed(&t.schema, &t.table))
            .collect();

        if !query.is_empty() {
            let terms: Vec<&str> = query.split_whitespace().collect();

            matches.retain(|t| {
                let full_name = format!("{}.{}", t.schema, t.table).to_ascii_lowercase();
                let table_name = t.table.to_ascii_lowercase();

                terms
                    .iter()
                    .all(|term| full_name.contains(term) || table_name.contains(term))
            });
        }

        matches.truncate(self.config.max_schema_results);

        println!("🔎 search_schema → {} coincidencias", matches.len());

        let mut output = String::new();

        output.push_str(&format!(
            "✓ TABLAS ENCONTRADAS: {}\n",
            matches.len()
        ));

        if matches.is_empty() {
            output.push_str("No se encontraron tablas.\n");
        } else {
            for table in &matches {
                output.push_str(&format!("  • {}.{}\n", table.schema, table.table));
            }
        }

        output.push_str(
            "\n→ SIGUIENTE PASO: Usa describe_table para ver columnas, \
             luego execute_read_query para obtener datos.",
        );

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

        let cols: Vec<ColumnInfo> = self
            .db
            .describe_table(&format!("{}.{}", schema, name))
            .await?;

        let mut output = format!("\n✓ ESTRUCTURA DE {}.{}\n\nCOLUMNAS:\n", schema, name);

        for col in &cols {
            output.push_str(&format!(
                "  • {} ({}){}\n",
                col.column,
                col.data_type,
                if col.nullable { " [NULLABLE]" } else { " [NO NULO]" }
            ));
        }

        output.push_str(
            "\n→ IMPORTANTE: Esta es solo la ESTRUCTURA. \n\
             Para obtener los DATOS REALES (filas, valores), \
             usa execute_read_query con un SELECT.",
        );

        Ok(limit_text(&output, self.config.max_tool_result_chars))
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

        let result = self.db.execute_read(sql).await?;

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
    let clean = s.replace('[', "").replace(']', "").replace('"', "");

    let parts: Vec<&str> = clean.split('.').collect();

    if parts.len() >= 2 {
        (
            parts[parts.len() - 2].to_string(),
            parts[parts.len() - 1].to_string(),
        )
    } else {
        ("dbo".into(), clean)
    }
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
