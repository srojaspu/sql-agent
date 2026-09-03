use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config as TdsConfig, QueryItem};
use tokio::{
    net::TcpStream,
    sync::Semaphore,
    time::{timeout, timeout_at},
};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::{
    config::Config,
    database::{
        schema::{ColumnMatch, ForeignKeyInfo, TableDetail},
        ColumnInfo, TableInfo,
    },
};

type TdsClient = Client<Compat<TcpStream>>;

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub row_count: usize,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

pub struct SqlServer {
    config: Config,
    query_gate: Semaphore,
}

impl SqlServer {
    pub fn new(config: Config) -> Self {
        Self {
            query_gate: Semaphore::new(config.max_concurrent_queries.max(1)),
            config,
        }
    }

    fn tds_config(&self) -> TdsConfig {
        let mut c = TdsConfig::new();
        c.host(&self.config.database_host);
        c.port(self.config.database_port);
        c.database(&self.config.database_name);
        c.authentication(AuthMethod::sql_server(
            &self.config.database_user,
            &self.config.database_password,
        ));
        if self.config.database_trust_cert {
            c.trust_cert();
        }
        c
    }

    async fn connect(&self) -> Result<TdsClient> {
        let config = self.tds_config();

        println!(
            "🔌 TCP → {}:{}",
            self.config.database_host, self.config.database_port
        );

        let tcp = timeout(
            Duration::from_secs(10),
            TcpStream::connect(config.get_addr()),
        )
        .await
        .context("Timeout TCP SQL Server")??;

        tcp.set_nodelay(true)?;
        println!("✅ TCP conectado");

        let client = timeout(
            Duration::from_secs(20),
            Client::connect(config, tcp.compat_write()),
        )
        .await
        .context("Timeout TLS/autenticación SQL Server")?
        .map_err(|e| anyhow::anyhow!("TLS/autenticación SQL Server: {e}"))?;

        println!("✅ Sesión SQL Server establecida");
        Ok(client)
    }

    pub async fn ping(&self) -> Result<(String, String)> {
        let mut c = self.connect().await?;
        self.verify_read_only(&mut c).await?;
        let rows = c
            .query(
                "SELECT DB_NAME() AS db_name, SUSER_SNAME() AS login_name",
                &[],
            )
            .await?
            .into_first_result()
            .await?;

        let db = rows
            .first()
            .and_then(|r| r.get::<&str, _>(0))
            .unwrap_or("")
            .to_string();
        let login = rows
            .first()
            .and_then(|r| r.get::<&str, _>(1))
            .unwrap_or("")
            .to_string();

        Ok((db, login))
    }

    async fn verify_read_only(&self, c: &mut TdsClient) -> Result<()> {
        let rows = c
            .query(
                "SELECT
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'INSERT') AS can_insert,
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'UPDATE') AS can_update,
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'DELETE') AS can_delete,
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'ALTER') AS can_alter,
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CONTROL') AS can_control",
                &[],
            )
            .await?
            .into_first_result()
            .await?;
        if let Some(row) = rows.first() {
            let perms = (0..5)
                .map(|i| row.get::<i32, _>(i).unwrap_or(0))
                .collect::<Vec<_>>();
            if perms.iter().any(|v| *v > 0) {
                anyhow::bail!("El login SQL tiene permisos de escritura/administración en la base de datos; agente abortado por seguridad");
            }
        }
        Ok(())
    }
    pub async fn list_tables(&self) -> Result<Vec<TableInfo>> {
        let mut c = self.connect().await?;
        let stream = c.query(list_tables_query(), &[]).await?;
        let rows = stream.into_first_result().await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let (Some(schema), Some(table), Some(table_type)) = (
                row.get::<&str, _>(0),
                row.get::<&str, _>(1),
                row.get::<&str, _>(2),
            ) {
                out.push(TableInfo {
                    schema: schema.into(),
                    table: table.into(),
                    table_type: table_type.into(),
                });
            }
        }
        Ok(out)
    }

    /// Search column names across the whole schema (LIKE-escaped, parameterized).
    pub async fn search_columns(&self, term: &str) -> Result<Vec<ColumnMatch>> {
        let mut c = self.connect().await?;
        let pattern = format!("%{}%", escape_like_pattern(term.trim()));
        let stream = c.query(search_columns_query(), &[&pattern]).await?;
        let rows = stream.into_first_result().await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let (Some(schema), Some(table), Some(column), Some(data_type)) = (
                row.get::<&str, _>(0),
                row.get::<&str, _>(1),
                row.get::<&str, _>(2),
                row.get::<&str, _>(3),
            ) {
                out.push(ColumnMatch {
                    schema: schema.into(),
                    table: table.into(),
                    column: column.into(),
                    data_type: data_type.into(),
                });
            }
        }
        Ok(out)
    }

    /// Enriched describe: columns + PK/FK + view definition + TOP 5 + COUNT(*).
    /// Works for zero-row tables (empty sample, count 0) and views (definition set).
    pub async fn describe_table_full(&self, table: &str) -> Result<TableDetail> {
        let (schema, name) = split_table_name(table);
        let columns = self.describe_table(table).await?;

        let mut c = self.connect().await?;

        // Primary keys
        let stream = c
            .query(
                "SELECT kcu.COLUMN_NAME \
                 FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
                 JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
                   ON tc.CONSTRAINT_NAME=kcu.CONSTRAINT_NAME \
                  AND tc.TABLE_SCHEMA=kcu.TABLE_SCHEMA \
                 WHERE tc.TABLE_SCHEMA=@P1 AND tc.TABLE_NAME=@P2 \
                   AND tc.CONSTRAINT_TYPE='PRIMARY KEY' \
                 ORDER BY kcu.ORDINAL_POSITION",
                &[&schema, &name],
            )
            .await?;
        let pk_rows = stream.into_first_result().await?;
        let mut primary_keys = Vec::new();
        for row in &pk_rows {
            if let Some(col) = row.get::<&str, _>(0) {
                primary_keys.push(col.to_owned());
            }
        }

        // Foreign keys
        let stream = c
            .query(
                "SELECT kcu.COLUMN_NAME,kcu2.TABLE_SCHEMA,kcu2.TABLE_NAME,kcu2.COLUMN_NAME \
                 FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS rc \
                 JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
                   ON rc.CONSTRAINT_NAME=kcu.CONSTRAINT_NAME \
                 JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu2 \
                   ON rc.UNIQUE_CONSTRAINT_NAME=kcu2.CONSTRAINT_NAME \
                  AND kcu.ORDINAL_POSITION=kcu2.ORDINAL_POSITION \
                 WHERE kcu.TABLE_SCHEMA=@P1 AND kcu.TABLE_NAME=@P2 \
                 ORDER BY kcu.ORDINAL_POSITION",
                &[&schema, &name],
            )
            .await?;
        let fk_rows = stream.into_first_result().await?;
        let mut foreign_keys = Vec::new();
        for row in &fk_rows {
            if let (Some(col), Some(rs), Some(rt), Some(rc)) = (
                row.get::<&str, _>(0),
                row.get::<&str, _>(1),
                row.get::<&str, _>(2),
                row.get::<&str, _>(3),
            ) {
                foreign_keys.push(ForeignKeyInfo {
                    column: col.into(),
                    ref_schema: rs.into(),
                    ref_table: rt.into(),
                    ref_column: rc.into(),
                });
            }
        }

        // View definition (None for base tables)
        let stream = c
            .query(
                "SELECT VIEW_DEFINITION FROM INFORMATION_SCHEMA.VIEWS \
                 WHERE TABLE_SCHEMA=@P1 AND TABLE_NAME=@P2",
                &[&schema, &name],
            )
            .await?;
        let view_rows = stream.into_first_result().await?;
        let view_definition = view_rows
            .first()
            .and_then(|r| r.get::<&str, _>(0))
            .map(str::to_owned);

        // TOP 5 sample + COUNT(*); identifiers escaped by ]-doubling.
        let qualified = format!("{}.{}", escape_ident(&schema), escape_ident(&name));
        let sample_sql = format!("SELECT TOP 5 * FROM {qualified}");
        let mut sample_rows = Vec::new();
        {
            let stream = c.query(sample_sql.as_str(), &[]).await?;
            let mut sample_stream = stream;
            while let Some(item) = sample_stream.try_next().await? {
                if let QueryItem::Row(row) = item {
                    let mut obj = serde_json::Map::new();
                    for (i, col) in row.columns().iter().enumerate() {
                        obj.insert(col.name().to_owned(), cell_to_json(&row, i));
                    }
                    sample_rows.push(Value::Object(obj));
                }
            }
        }

        let count_sql = format!("SELECT COUNT(*) FROM {qualified}");
        let count_rows = c
            .query(count_sql.as_str(), &[])
            .await?
            .into_first_result()
            .await?;
        let row_count = count_rows
            .first()
            .and_then(|r| r.get::<i32, _>(0))
            .unwrap_or(0) as i64;

        Ok(TableDetail {
            columns,
            primary_keys,
            foreign_keys,
            view_definition,
            sample_rows,
            row_count,
        })
    }

    pub async fn describe_table(&self, table: &str) -> Result<Vec<ColumnInfo>> {
        let (schema, name) = split_table_name(table);
        let mut c = self.connect().await?;
        let stream = c
            .query(
                "SELECT COLUMN_NAME,DATA_TYPE,IS_NULLABLE,ORDINAL_POSITION \
             FROM INFORMATION_SCHEMA.COLUMNS \
             WHERE TABLE_SCHEMA=@P1 AND TABLE_NAME=@P2 \
             ORDER BY ORDINAL_POSITION",
                &[&schema, &name],
            )
            .await?;
        let rows = stream.into_first_result().await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let (Some(column), Some(data_type), Some(nullable), Some(ordinal)) = (
                row.get::<&str, _>(0),
                row.get::<&str, _>(1),
                row.get::<&str, _>(2),
                row.get::<i32, _>(3),
            ) {
                out.push(ColumnInfo {
                    column: column.into(),
                    data_type: data_type.into(),
                    nullable: nullable.eq_ignore_ascii_case("YES"),
                    ordinal,
                });
            }
        }
        Ok(out)
    }

    pub async fn execute_read(&self, sql: &str) -> Result<QueryResult> {
        let _permit = self.query_gate.acquire().await?;
        let mut c = self.connect().await?;
        self.verify_read_only(&mut c).await?;

        // These are session-scoped safety/performance controls. The application
        // SQL itself has already passed the AST validator before reaching here.
        c.simple_query("SET NOCOUNT ON; SET XACT_ABORT ON; SET LOCK_TIMEOUT 5000; SET TRANSACTION ISOLATION LEVEL READ COMMITTED;")
            .await?
            .into_results()
            .await?;

        let mut stream = timeout(
            Duration::from_secs(self.config.query_timeout_seconds),
            c.query(sql, &[]),
        )
        .await
        .context("Timeout iniciando consulta SQL")??;

        // Consume como máximo max_rows + 1 filas. Esto evita cargar un resultset
        // potencialmente enorme en memoria solo para descubrir que estaba truncado.
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.config.query_timeout_seconds);
        let mut result = Vec::with_capacity(self.config.max_rows);
        let mut truncated = false;
        while result.len() <= self.config.max_rows {
            let next = timeout_at(deadline, stream.try_next())
                .await
                .context("Timeout leyendo resultado SQL")??;
            let Some(item) = next else {
                break;
            };
            let QueryItem::Row(row) = item else {
                // Metadata is not a data row. Other query items are ignored.
                continue;
            };
            if result.len() == self.config.max_rows {
                truncated = true;
                break;
            }
            let mut obj = serde_json::Map::new();
            for (i, col) in row.columns().iter().enumerate() {
                obj.insert(col.name().to_owned(), cell_to_json(&row, i));
            }
            result.push(Value::Object(obj));
        }
        Ok(QueryResult {
            row_count: result.len(),
            truncated,
            rows: result,
        })
    }
}

/// Query listing every visible table AND view with its type.
fn list_tables_query() -> &'static str {
    "SELECT TABLE_SCHEMA,TABLE_NAME,TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES \
     ORDER BY TABLE_SCHEMA,TABLE_NAME"
}

/// Parameterized column search; the caller passes a pre-escaped LIKE pattern as @P1.
fn search_columns_query() -> &'static str {
    "SELECT c.TABLE_SCHEMA,c.TABLE_NAME,c.COLUMN_NAME,c.DATA_TYPE \
     FROM INFORMATION_SCHEMA.COLUMNS c \
     WHERE c.COLUMN_NAME LIKE @P1 \
     ORDER BY c.TABLE_SCHEMA,c.TABLE_NAME,c.ORDINAL_POSITION"
}

/// Escape T-SQL LIKE wildcards so the term matches literally.
/// Bracket-escaping needs no ESCAPE clause: % -> [%], _ -> [_], [ -> [[], ] -> []].
fn escape_like_pattern(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    for ch in term.chars() {
        match ch {
            '%' => out.push_str("[%]"),
            '_' => out.push_str("[_]"),
            '[' => out.push_str("[[]"),
            ']' => out.push_str("[]]"),
            _ => out.push(ch),
        }
    }
    out
}

/// Quote a T-SQL identifier by wrapping in brackets, doubling any closing bracket.
fn escape_ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

fn split_table_name(table: &str) -> (String, String) {
    let cleaned = table.trim().replace('[', "").replace(']', "");
    let p: Vec<&str> = cleaned.split('.').collect();
    if p.len() >= 2 {
        (p[p.len() - 2].to_owned(), p[p.len() - 1].to_owned())
    } else {
        ("dbo".to_owned(), cleaned)
    }
}

fn cell_to_json(row: &tiberius::Row, idx: usize) -> Value {
    // Intentar como String primero
    if let Ok(Some(v)) = row.try_get::<&str, _>(idx) {
        return json!(v);
    }
    // Intentar como i32
    if let Ok(Some(v)) = row.try_get::<i32, _>(idx) {
        return json!(v);
    }
    // Intentar como i16
    if let Ok(Some(v)) = row.try_get::<i16, _>(idx) {
        return json!(v);
    }
    // Intentar como i64
    if let Ok(Some(v)) = row.try_get::<i64, _>(idx) {
        return json!(v);
    }
    // Intentar como u8
    if let Ok(Some(v)) = row.try_get::<u8, _>(idx) {
        return json!(v);
    }
    // Intentar como f32
    if let Ok(Some(v)) = row.try_get::<f32, _>(idx) {
        return json!(v);
    }
    // Intentar como f64
    if let Ok(Some(v)) = row.try_get::<f64, _>(idx) {
        return json!(v);
    }
    // Intentar como bool
    if let Ok(Some(v)) = row.try_get::<bool, _>(idx) {
        return json!(v);
    }
    // Intentar como UUID
    if let Ok(Some(v)) = row.try_get::<uuid::Uuid, _>(idx) {
        return json!(v.to_string());
    }
    // Intentar como NaiveDateTime
    if let Ok(Some(v)) = row.try_get::<NaiveDateTime, _>(idx) {
        return json!(v.to_string());
    }
    // Intentar como DateTime<Utc>
    if let Ok(Some(v)) = row.try_get::<DateTime<Utc>, _>(idx) {
        return json!(v.to_rfc3339());
    }
    // Intentar como NaiveDate
    if let Ok(Some(v)) = row.try_get::<NaiveDate, _>(idx) {
        return json!(v.to_string());
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_like_leaves_plain_text_untouched() {
        assert_eq!(escape_like_pattern("email"), "email");
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like_pattern("100%_off"), "100[%][_]off");
    }

    #[test]
    fn escape_like_escapes_brackets() {
        assert_eq!(escape_like_pattern("a[b]c"), "a[[]b[]]c");
    }

    #[test]
    fn escape_ident_brackets_and_doubles_close() {
        assert_eq!(escape_ident("dbo"), "[dbo]");
        assert_eq!(escape_ident("we]ird"), "[we]]ird]");
    }

    #[test]
    fn search_columns_query_filters_by_like_param() {
        let q = search_columns_query();
        assert!(
            q.contains("COLUMN_NAME LIKE @P1"),
            "search must use parameterized LIKE, got: {q}"
        );
        assert!(
            q.contains("INFORMATION_SCHEMA.COLUMNS"),
            "search must read COLUMNS, got: {q}"
        );
    }

    #[test]
    fn list_tables_query_selects_table_type() {
        let q = list_tables_query();
        assert!(
            q.contains("TABLE_TYPE"),
            "list must select TABLE_TYPE, got: {q}"
        );
    }
}
