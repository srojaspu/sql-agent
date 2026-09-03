use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tiberius::{numeric::Numeric, AuthMethod, Client, Config as TdsConfig, QueryItem};
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
    util::split_table_name,
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

/// Pure, testable core of `verify_read_only` (fail-closed).
///
/// `perms` holds the five database write/admin flags (INSERT/UPDATE/DELETE/
/// ALTER/CONTROL); the role flags deny privileged logins outright. An empty
/// or short `perms` slice means the permission query returned nothing usable,
/// which must deny, never allow.
fn assess_read_only(
    perms: &[i32],
    sysadmin: i32,
    db_owner: i32,
    control_server: i32,
) -> Result<()> {
    check_write_perms(perms)?;
    check_privileged_roles(sysadmin, db_owner, control_server)
}

/// Deny when any database-level write/admin permission is granted.
/// Fails closed on empty/incomplete results.
fn check_write_perms(perms: &[i32]) -> Result<()> {
    if perms.len() < 5 {
        anyhow::bail!("Permission check returned incomplete/empty result; failing closed");
    }
    if perms.iter().any(|v| *v > 0) {
        anyhow::bail!("El login SQL tiene permisos de escritura/administración en la base de datos; agente abortado por seguridad");
    }
    Ok(())
}

/// Deny logins holding dangerous server/database roles.
fn check_privileged_roles(sysadmin: i32, db_owner: i32, control_server: i32) -> Result<()> {
    if sysadmin > 0 {
        anyhow::bail!("El login SQL es miembro de sysadmin; agente abortado por seguridad");
    }
    if db_owner > 0 {
        anyhow::bail!("El login SQL es miembro de db_owner; agente abortado por seguridad");
    }
    if control_server > 0 {
        anyhow::bail!("El login SQL tiene CONTROL SERVER; agente abortado por seguridad");
    }
    Ok(())
}

impl SqlServer {
    pub fn new(config: Config) -> Self {
        Self {
            query_gate: Semaphore::new(config.max_concurrent_queries.max(1)),
            config,
        }
    }

    /// TCP connect timeout, driven by `OLLAMA_CONNECT_TIMEOUT_SECONDS`.
    /// Reuses the existing connect-timeout knob (no new env vars) and falls
    /// back to the historical 10s when the config value is 0.
    fn tcp_connect_timeout(&self) -> Duration {
        let secs = if self.config.ollama_connect_timeout_seconds > 0 {
            self.config.ollama_connect_timeout_seconds
        } else {
            10
        };
        Duration::from_secs(secs)
    }

    /// TLS/handshake timeout, driven by `QUERY_TIMEOUT_SECONDS`.
    /// Falls back to the historical 20s when the config value is 0.
    fn tls_handshake_timeout(&self) -> Duration {
        let secs = if self.config.query_timeout_seconds > 0 {
            self.config.query_timeout_seconds
        } else {
            20
        };
        Duration::from_secs(secs)
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

        tracing::debug!(
            "🔌 TCP → {}:{}",
            self.config.database_host, self.config.database_port
        );

        let tcp = timeout(
            self.tcp_connect_timeout(),
            TcpStream::connect(config.get_addr()),
        )
        .await
        .context("Timeout TCP SQL Server")??;

        tcp.set_nodelay(true)?;
        tracing::debug!("✅ TCP conectado");

        let client = timeout(
            self.tls_handshake_timeout(),
            Client::connect(config, tcp.compat_write()),
        )
        .await
        .context("Timeout TLS/autenticación SQL Server")?
        .map_err(|e| anyhow::anyhow!("TLS/autenticación SQL Server: {e}"))?;

        tracing::debug!("✅ Sesión SQL Server establecida");
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
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CONTROL') AS can_control,
                IS_SRVROLEMEMBER('sysadmin') AS is_sysadmin,
                IS_MEMBER('db_owner') AS is_db_owner,
                HAS_PERMS_BY_NAME(NULL, NULL, 'CONTROL SERVER') AS can_control_server",
                &[],
            )
            .await?
            .into_first_result()
            .await?;
        let Some(row) = rows.first() else {
            anyhow::bail!("Permission check returned no rows; failing closed");
        };
        let perms = (0..5)
            .map(|i| row.get::<i32, _>(i).unwrap_or(0))
            .collect::<Vec<_>>();
        let sysadmin = row.get::<i32, _>(5).unwrap_or(0);
        let db_owner = row.get::<i32, _>(6).unwrap_or(0);
        let control_server = row.get::<i32, _>(7).unwrap_or(0);
        assess_read_only(&perms, sysadmin, db_owner, control_server)
    }
    pub async fn list_tables(&self) -> Result<Vec<TableInfo>> {
        let mut c = self.connect().await?;
        self.verify_read_only(&mut c).await?;
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
        self.verify_read_only(&mut c).await?;
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

    /// Enriched describe: columns + PK/FK + view definition + TOP 5 + bounded COUNT(*).
    /// Works for zero-row tables (empty sample, count 0) and views (definition set).
    /// Uses a SINGLE connection for all six sections (columns/PK/FK/VIEW/TOP5/COUNT)
    /// and wraps every query in `query_timeout_seconds` so a hung INFORMATION_SCHEMA
    /// or sample scan cannot stall describe; only COUNT degrades to -1 (unknown).
    /// COUNT is capped (TOP) so huge views (e.g. 436k rows hung dev-DB on full COUNT)
    /// still return structure; on timeout/cap-exhaustion row_count is -1 (unknown)
    /// instead of failing.
    pub async fn describe_table_full(&self, table: &str) -> Result<TableDetail> {
        let (schema, name) = split_table_name(table);
        let qt = Duration::from_secs(self.config.query_timeout_seconds.max(1));
        // Single connection shared by every section below (no re-connect per query).
        let mut c = self.connect().await?;
        self.verify_read_only(&mut c).await?;

        // Columns (same projection/parse as `describe_table`; inlined to share `c`).
        let stream = timeout(qt, c.query(describe_columns_query(), &[&schema, &name]))
            .await
            .context("describe columns timeout")??;
        let col_rows = timeout(qt, stream.into_first_result())
            .await
            .context("describe columns timeout")??;
        let columns = parse_column_rows(&col_rows);

        // Primary keys
        let stream = timeout(qt, c.query(describe_pk_query(), &[&schema, &name]))
            .await
            .context("describe PK timeout")??;
        let pk_rows = timeout(qt, stream.into_first_result())
            .await
            .context("describe PK timeout")??;
        let mut primary_keys = Vec::new();
        for row in &pk_rows {
            if let Some(col) = row.get::<&str, _>(0) {
                primary_keys.push(col.to_owned());
            }
        }

        // Foreign keys
        let stream = timeout(qt, c.query(describe_fk_query(), &[&schema, &name]))
            .await
            .context("describe FK timeout")??;
        let fk_rows = timeout(qt, stream.into_first_result())
            .await
            .context("describe FK timeout")??;
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
        let stream = timeout(qt, c.query(describe_view_query(), &[&schema, &name]))
            .await
            .context("describe VIEW timeout")??;
        let view_rows = timeout(qt, stream.into_first_result())
            .await
            .context("describe VIEW timeout")??;
        let view_definition = view_rows
            .first()
            .and_then(|r| r.get::<&str, _>(0))
            .map(str::to_owned);

        // TOP 5 sample + COUNT(*); identifiers escaped by ]-doubling.
        let qualified = format!("{}.{}", escape_ident(&schema), escape_ident(&name));
        let sample_sql = format!("SELECT TOP 5 * FROM {qualified}");
        let mut sample_rows = Vec::new();
        {
            let mut sample_stream = timeout(qt, c.query(sample_sql.as_str(), &[]))
                .await
                .context("describe sample timeout")??;
            loop {
                let next = timeout(qt, sample_stream.try_next())
                    .await
                    .context("describe sample timeout")??;
                let Some(item) = next else {
                    break;
                };
                if let QueryItem::Row(row) = item {
                    let mut obj = serde_json::Map::new();
                    for (i, col) in row.columns().iter().enumerate() {
                        obj.insert(col.name().to_owned(), cell_to_json(&row, i));
                    }
                    sample_rows.push(Value::Object(obj));
                }
            }
        }

        let count_sql = describe_count_sql(&qualified);
        // Bounded COUNT with timeout: a full COUNT(*) hung dev-DB on a 436k-row
        // view, so the scan is TOP-capped and wrapped in query_timeout_seconds.
        // COUNT failure must never fail describe — structure/sample above are kept.
        let count_timeout = qt;
        let count_outcome: Result<Option<i64>> = async {
            let stream = timeout(count_timeout, c.query(count_sql.as_str(), &[]))
                .await
                .context("describe COUNT timeout")??;
            let rows = timeout(count_timeout, stream.into_first_result())
                .await
                .context("describe COUNT timeout")??;
            Ok(rows
                .first()
                .and_then(|r| r.get::<i32, _>(0))
                .map(|v| v as i64))
        }
        .await;
        if let Err(e) = &count_outcome {
            tracing::warn!("describe COUNT capped/timed out for {qualified}: {e:#}");
        }
        let row_count = resolve_describe_row_count(count_outcome);

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
        self.verify_read_only(&mut c).await?;
        let stream = c
            .query(describe_columns_query(), &[&schema, &name])
            .await?;
        let rows = stream.into_first_result().await?;
        Ok(parse_column_rows(&rows))
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

/// Columns section of describe (shared by `describe_table` and
/// `describe_table_full` so both return identical column projections).
fn describe_columns_query() -> &'static str {
    "SELECT COLUMN_NAME,DATA_TYPE,IS_NULLABLE,ORDINAL_POSITION \
     FROM INFORMATION_SCHEMA.COLUMNS \
     WHERE TABLE_SCHEMA=@P1 AND TABLE_NAME=@P2 \
     ORDER BY ORDINAL_POSITION"
}

/// Primary-key section of describe (parameterized by schema/table).
fn describe_pk_query() -> &'static str {
    "SELECT kcu.COLUMN_NAME \
     FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
     JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
       ON tc.CONSTRAINT_NAME=kcu.CONSTRAINT_NAME \
      AND tc.TABLE_SCHEMA=kcu.TABLE_SCHEMA \
     WHERE tc.TABLE_SCHEMA=@P1 AND tc.TABLE_NAME=@P2 \
       AND tc.CONSTRAINT_TYPE='PRIMARY KEY' \
     ORDER BY kcu.ORDINAL_POSITION"
}

/// Foreign-key section of describe (parameterized by schema/table).
fn describe_fk_query() -> &'static str {
    "SELECT kcu.COLUMN_NAME,kcu2.TABLE_SCHEMA,kcu2.TABLE_NAME,kcu2.COLUMN_NAME \
     FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS rc \
     JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
       ON rc.CONSTRAINT_NAME=kcu.CONSTRAINT_NAME \
     JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu2 \
       ON rc.UNIQUE_CONSTRAINT_NAME=kcu2.CONSTRAINT_NAME \
      AND kcu.ORDINAL_POSITION=kcu2.ORDINAL_POSITION \
     WHERE kcu.TABLE_SCHEMA=@P1 AND kcu.TABLE_NAME=@P2 \
     ORDER BY kcu.ORDINAL_POSITION"
}

/// View-definition section of describe (parameterized by schema/table).
fn describe_view_query() -> &'static str {
    "SELECT VIEW_DEFINITION FROM INFORMATION_SCHEMA.VIEWS \
     WHERE TABLE_SCHEMA=@P1 AND TABLE_NAME=@P2"
}

/// Shared column-row parser so `describe_table` and `describe_table_full`
/// map INFORMATION_SCHEMA.COLUMNS identically.
fn parse_column_rows(rows: &[tiberius::Row]) -> Vec<ColumnInfo> {
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
    out
}

/// Max rows scanned by the describe COUNT. A full COUNT(*) hung dev-DB on a
/// 436k-row view, so describe counts at most CAP+1 rows (fast bounded scan).
pub const DESCRIBE_COUNT_CAP: i64 = 100_000;

/// Build the bounded COUNT query for describe: counts up to CAP+1 rows of an
/// already-escaped qualified name. A result of CAP+1 means "more than CAP".
pub fn describe_count_sql(qualified: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM (SELECT TOP {} * FROM {qualified}) AS _describe_cnt",
        DESCRIBE_COUNT_CAP + 1
    )
}

/// Resolve the describe COUNT outcome into a displayable row_count.
/// Ok(Some(n)) -> n, Ok(None) -> 0, Err (timeout/driver) -> -1 (unknown).
/// The -1 sentinel keeps the describe path returning structure instead of failing.
pub fn resolve_describe_row_count(outcome: Result<Option<i64>>) -> i64 {
    match outcome {
        Ok(Some(n)) => n,
        Ok(None) => 0,
        Err(_) => -1,
    }
}

/// Format a TDS decimal/numeric value with exact decimal placement.
///
/// tiberius stores decimals as an i128 plus a scale, and its own Display
/// mishandles negatives (-1999 at scale 2 renders as "-19.-99"), so the sign
/// is applied explicitly around the absolute digits. The result is returned
/// as a JSON string because f64 cannot hold every DECIMAL exactly.
fn format_numeric_value(n: Numeric) -> String {
    let scale = n.scale() as usize;
    if scale == 0 {
        return n.value().to_string();
    }
    let abs = n.value().unsigned_abs();
    let factor = 10u128.pow(scale as u32);
    let fraction = format!("{:0>width$}", abs % factor, width = scale);
    format!(
        "{}{}.{}",
        if n.value() < 0 { "-" } else { "" },
        abs / factor,
        fraction
    )
}

/// Hand-rolled hex encoding for binary/varbinary cells.
///
/// No extra dependency is pulled in for this single use; bytes render as
/// `0x`-prefixed uppercase hex so `0xAB` round-trips recognisably.
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Sentinel for cells whose TDS type has no JSON mapping.
///
/// Reached only when every typed probe failed: SQL NULL always satisfies at
/// least one probe with `Ok(None)`, so falling through here means the driver
/// genuinely cannot decode the value. Collapsing that to `null` would lie to
/// the LLM; the column type is named instead.
fn unsupported_sentinel(column_type: tiberius::ColumnType) -> Value {
    json!(format!("[UNSUPPORTED: {:?}]", column_type))
}

fn cell_to_json(row: &tiberius::Row, idx: usize) -> Value {
    // A probe `Err` means "wrong type, keep looking"; a decoded SQL NULL
    // (`Ok(None)`) is remembered so genuine NULLs stay JSON null while values
    // no probe understands fall through to the sentinel below.
    let mut saw_null = false;
    macro_rules! probe {
        ($ty:ty, $map:expr) => {
            match row.try_get::<$ty, _>(idx) {
                Ok(Some(v)) => return $map(v),
                Ok(None) => {
                    saw_null = true;
                }
                Err(_) => {}
            }
        };
    }

    // String first: most text-like columns land here.
    probe!(&str, |v: &str| json!(v));
    // Exact decimal/numeric display as string; never via f64.
    probe!(Numeric, |v: Numeric| json!(format_numeric_value(v)));
    // Integer types.
    probe!(i32, |v: i32| json!(v));
    probe!(i16, |v: i16| json!(v));
    probe!(i64, |v: i64| json!(v));
    probe!(u8, |v: u8| json!(v));
    // Floats. NOTE (MONEY limit): the TDS driver decodes MONEY/SMALLMONEY as
    // f64 (raw value / 1e4) before this function ever runs, so MONEY arrives
    // here indistinguishable from FLOAT and already subject to binary-float
    // rounding. Queries needing exact money arithmetic must CAST to DECIMAL
    // in SQL; this limit is documented rather than hidden.
    probe!(f32, |v: f32| json!(v));
    probe!(f64, |v: f64| json!(v));
    // Boolean.
    probe!(bool, |v: bool| json!(v));
    // Binary data as hand-rolled hex, never as lossy text.
    probe!(&[u8], |v: &[u8]| json!(bytes_to_hex(v)));
    // UUID as canonical string.
    probe!(uuid::Uuid, |v: uuid::Uuid| json!(v.to_string()));
    // Temporal types as strings (`NaiveTime` covers SQL TIME).
    probe!(NaiveDateTime, |v: NaiveDateTime| json!(v.to_string()));
    probe!(DateTime<Utc>, |v: DateTime<Utc>| json!(v.to_rfc3339()));
    probe!(NaiveDate, |v: NaiveDate| json!(v.to_string()));
    probe!(NaiveTime, |v: NaiveTime| json!(v.to_string()));
    if saw_null {
        return Value::Null;
    }
    let column_type = row
        .columns()
        .get(idx)
        .map(|c| c.column_type())
        .unwrap_or(tiberius::ColumnType::Null);
    unsupported_sentinel(column_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_permission_result_fails_closed() {
        assert!(
            assess_read_only(&[], 0, 0, 0).is_err(),
            "empty permission result must fail closed"
        );
        assert!(
            check_write_perms(&[]).is_err(),
            "empty perms must fail closed"
        );
        assert!(
            check_write_perms(&[0, 0, 0]).is_err(),
            "incomplete perms must fail closed"
        );
    }

    #[test]
    fn discovery_without_permissions_fails_closed() {
        // Read-only login: all write flags off, no privileged roles.
        assert!(assess_read_only(&[0, 0, 0, 0, 0], 0, 0, 0).is_ok());
        // Any write/admin grant denies discovery.
        assert!(assess_read_only(&[0, 1, 0, 0, 0], 0, 0, 0).is_err());
        assert!(assess_read_only(&[0, 0, 0, 0, 1], 0, 0, 0).is_err());
    }

    #[test]
    fn privileged_roles_are_denied() {
        assert!(check_privileged_roles(0, 0, 0).is_ok());
        assert!(check_privileged_roles(1, 0, 0).is_err(), "sysadmin must be denied");
        assert!(check_privileged_roles(0, 1, 0).is_err(), "db_owner must be denied");
        assert!(
            check_privileged_roles(0, 0, 1).is_err(),
            "CONTROL SERVER must be denied"
        );
        assert!(
            assess_read_only(&[0, 0, 0, 0, 0], 1, 0, 0).is_err(),
            "sysadmin must deny even with no write flags"
        );
    }

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

    #[test]
    fn numeric_formats_exact_with_two_places() {
        let n = tiberius::numeric::Numeric::new_with_scale(1999, 2);
        assert_eq!(format_numeric_value(n), "19.99");
    }

    #[test]
    fn numeric_formats_negative_exact() {
        // tiberius Display renders this as "-19.-99"; exact encoding must not.
        let n = tiberius::numeric::Numeric::new_with_scale(-1999, 2);
        assert_eq!(format_numeric_value(n), "-19.99");
    }

    #[test]
    fn numeric_formats_fraction_with_leading_zero() {
        assert_eq!(
            format_numeric_value(tiberius::numeric::Numeric::new_with_scale(5, 2)),
            "0.05"
        );
    }

    #[test]
    fn numeric_formats_scale_zero_as_integer() {
        assert_eq!(
            format_numeric_value(tiberius::numeric::Numeric::new_with_scale(100, 0)),
            "100"
        );
    }

    #[test]
    fn bytes_to_hex_single_byte() {
        assert_eq!(bytes_to_hex(&[0xAB]), "0xAB");
    }

    #[test]
    fn bytes_to_hex_preserves_leading_zeroes() {
        assert_eq!(bytes_to_hex(&[0x00, 0x0F]), "0x000F");
    }

    #[test]
    fn bytes_to_hex_empty_is_prefix_only() {
        assert_eq!(bytes_to_hex(&[]), "0x");
    }

    #[test]
    fn unsupported_sentinel_names_type_and_is_not_null() {
        let v = unsupported_sentinel(tiberius::ColumnType::Timen);
        assert_ne!(
            v,
            Value::Null,
            "unsupported types must never collapse to silent NULL"
        );
        let s = v.as_str().unwrap_or_default().to_owned();
        assert!(
            s.contains("[UNSUPPORTED"),
            "sentinel must carry [UNSUPPORTED marker, got: {s}"
        );
        assert!(
            s.contains("Timen"),
            "sentinel must name the column type, got: {s}"
        );
    }

    #[test]
    fn describe_count_sql_is_bounded_with_top_cap() {
        let q = describe_count_sql("[dbo].[BigView]");
        assert!(
            q.contains("TOP 100001"),
            "COUNT must be TOP-capped to avoid full scans, got: {q}"
        );
        assert!(
            q.contains("[dbo].[BigView]"),
            "qualified name must be preserved, got: {q}"
        );
        assert!(q.contains("COUNT(*)"), "must still count, got: {q}");
    }

    #[test]
    fn resolve_describe_row_count_ok_some_returns_value() {
        assert_eq!(resolve_describe_row_count(Ok(Some(42))), 42);
        // CAP+1 means "more than CAP" — still a usable count, path stays alive.
        assert_eq!(
            resolve_describe_row_count(Ok(Some(DESCRIBE_COUNT_CAP + 1))),
            DESCRIBE_COUNT_CAP + 1
        );
    }

    #[test]
    fn resolve_describe_row_count_none_is_zero_and_err_is_unknown() {
        assert_eq!(resolve_describe_row_count(Ok(None)), 0);
        let err: Result<Option<i64>> = Err(anyhow::anyhow!("describe COUNT timeout"));
        assert_eq!(
            resolve_describe_row_count(err),
            -1,
            "timeout must yield unknown (-1), never fail describe"
        );
    }

    #[test]
    fn describe_sections_stay_parameterized_and_complete() {
        // P1a-5: describe_table_full must keep the same six sections over one
        // connection: columns + PK + FK + VIEW + TOP5 sample + bounded COUNT.
        // Every INFORMATION_SCHEMA section stays parameterized (@P1/@P2) so no
        // table name is ever interpolated; only the TOP5/COUNT qualified name
        // is escaped via escape_ident.
        for q in [
            describe_columns_query(),
            describe_pk_query(),
            describe_fk_query(),
            describe_view_query(),
        ] {
            assert!(
                q.contains("@P1") && q.contains("@P2"),
                "describe section must stay parameterized, got: {q}"
            );
        }
        assert!(
            describe_columns_query().contains("INFORMATION_SCHEMA.COLUMNS"),
            "columns section must read COLUMNS"
        );
        assert!(
            describe_pk_query().contains("PRIMARY KEY"),
            "PK section must filter PRIMARY KEY"
        );
        assert!(
            describe_fk_query().contains("REFERENTIAL_CONSTRAINTS"),
            "FK section must read REFERENTIAL_CONSTRAINTS"
        );
        assert!(
            describe_view_query().contains("INFORMATION_SCHEMA.VIEWS"),
            "view section must read VIEWS"
        );
    }

    #[test]
    fn describe_columns_query_matches_single_table_projection() {
        // describe_table and describe_table_full share this projection, so the
        // single-connection refactor cannot drift column sections apart.
        let q = describe_columns_query();
        for col in ["COLUMN_NAME", "DATA_TYPE", "IS_NULLABLE", "ORDINAL_POSITION"] {
            assert!(q.contains(col), "columns query must select {col}, got: {q}");
        }
    }

    #[test]
    fn connect_timeouts_follow_config_with_fallback() {
        use crate::config::Config;
        use std::collections::HashMap;
        fn cfg_with(query: &str, connect: &str) -> Config {
            let mut m = HashMap::new();
            m.insert("DATABASE_HOST".into(), "localhost".into());
            m.insert("DATABASE_NAME".into(), "TestDB".into());
            m.insert("DATABASE_USER".into(), "u".into());
            m.insert("DATABASE_PASSWORD".into(), "p".into());
            m.insert("QUERY_TIMEOUT_SECONDS".into(), query.into());
            m.insert("OLLAMA_CONNECT_TIMEOUT_SECONDS".into(), connect.into());
            Config::from_map(&m).expect("config should parse")
        }
        // Defaults stay intact at the Config layer.
        let defaults = cfg_with("30", "5");
        assert_eq!(defaults.query_timeout_seconds, 30);
        assert_eq!(defaults.ollama_connect_timeout_seconds, 5);
        // SqlServer follows them: TCP <- connect knob, TLS <- query knob.
        let srv = SqlServer::new(defaults);
        assert_eq!(srv.tcp_connect_timeout(), Duration::from_secs(5));
        assert_eq!(srv.tls_handshake_timeout(), Duration::from_secs(30));
        // Overrides are respected.
        let over = SqlServer::new(cfg_with("7", "2"));
        assert_eq!(over.tcp_connect_timeout(), Duration::from_secs(2));
        assert_eq!(over.tls_handshake_timeout(), Duration::from_secs(7));
        // Zero falls back to the historical 10s/20s instead of a 0s timeout.
        let fb = SqlServer::new(cfg_with("0", "0"));
        assert_eq!(fb.tcp_connect_timeout(), Duration::from_secs(10));
        assert_eq!(fb.tls_handshake_timeout(), Duration::from_secs(20));
    }

    #[test]
    fn money_tds_cap_must_cast_to_decimal_for_exactness() {
        // TDS decodes MONEY/SMALLMONEY as f64 (raw/1e4) — binary-float rounding
        // applies before cell_to_json runs. Exact money arithmetic must CAST to
        // DECIMAL in SQL; DECIMAL arrives as Numeric and stays exact here.
        // This test pins the exact DECIMAL(19,4) path the CAST workaround relies on.
        let n = tiberius::numeric::Numeric::new_with_scale(199900, 4);
        assert_eq!(format_numeric_value(n), "19.9900");
    }
}
