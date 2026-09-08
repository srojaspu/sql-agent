use anyhow::{Context, Result};
use async_trait::async_trait;
use bb8::{ManageConnection, Pool, PooledConnection};
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config as TdsConfig, QueryItem};
use tokio::{
    net::TcpStream,
    sync::{OnceCell, Semaphore},
    time::{timeout, timeout_at},
};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::{
    config::Config,
    database::{
        describe::{
            describe_count_sql, describe_sample_sql, resolve_describe_row_count, safe_columns,
        },
        queries::{
            describe_columns_query, describe_fk_query, describe_pk_query, describe_view_query,
            escape_ident, escape_like_pattern, list_tables_query, search_columns_query,
        },
        repository::DatabaseRepository,
        rows::{cell_to_json, parse_column_rows},
        schema::{ColumnMatch, ForeignKeyInfo, TableDetail},
        ColumnInfo, TableInfo,
    },
    security::ValidatedSql,
    util::split_table_name,
};

type TdsClient = Client<Compat<TcpStream>>;
type TdsPool = Pool<TdsConnectionManager>;

#[derive(Clone)]
struct TdsConnectionManager {
    config: Config,
}

impl ManageConnection for TdsConnectionManager {
    type Connection = TdsClient;
    type Error = anyhow::Error;

    fn connect(
        &self,
    ) -> impl std::future::Future<Output = Result<Self::Connection, Self::Error>> + Send {
        connect_tds(&self.config)
    }

    async fn is_valid(&self, connection: &mut Self::Connection) -> Result<(), Self::Error> {
        connection
            .simple_query("SELECT 1")
            .await?
            .into_results()
            .await?;
        Ok(())
    }

    fn has_broken(&self, _connection: &mut Self::Connection) -> bool {
        false
    }
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub row_count: usize,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

pub struct SqlServer {
    config: Config,
    query_gate: Semaphore,
    pool: OnceCell<TdsPool>,
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
            query_gate: Semaphore::new(config.limits.max_concurrent_queries.max(1)),
            pool: OnceCell::new(),
            config,
        }
    }

    async fn connection(&self) -> Result<PooledConnection<'_, TdsConnectionManager>> {
        let pool = self
            .pool
            .get_or_try_init(|| async {
                Pool::builder()
                    .max_size(self.config.limits.max_concurrent_queries.max(1) as u32)
                    .test_on_check_out(true)
                    .build(TdsConnectionManager {
                        config: self.config.clone(),
                    })
                    .await
                    .context("No se pudo crear el pool SQL Server")
            })
            .await?;
        pool.get()
            .await
            .map_err(|error| anyhow::anyhow!("No se pudo obtener conexión del pool: {error:?}"))
    }

    pub async fn ping(&self) -> Result<(String, String)> {
        let mut c = self.connection().await?;
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
        let mut c = self.connection().await?;
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
        let mut c = self.connection().await?;
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
        let qt = Duration::from_secs(self.config.limits.query_timeout_s.max(1));
        // Single connection shared by every section below (no re-connect per query).
        let mut c = self.connection().await?;
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

        // TOP 5 sample over safe columns only; identifiers escaped by
        // ]-doubling. When every column is sensitive, skip the fetch so no
        // value ever leaves the database (empty sample + redacted render).
        let qualified = format!("{}.{}", escape_ident(&schema), escape_ident(&name));
        let safe_cols = safe_columns(&columns);
        let mut sample_rows = Vec::new();
        if let Some(sample_sql) = describe_sample_sql(&qualified, &safe_cols) {
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
        let mut c = self.connection().await?;
        self.verify_read_only(&mut c).await?;
        let stream = c.query(describe_columns_query(), &[&schema, &name]).await?;
        let rows = stream.into_first_result().await?;
        Ok(parse_column_rows(&rows))
    }

    pub async fn execute_read(&self, validated: ValidatedSql) -> Result<QueryResult> {
        let sql = validated.as_str();
        let _permit = self.query_gate.acquire().await?;
        let mut c = self.connection().await?;
        self.verify_read_only(&mut c).await?;

        // These are session-scoped safety/performance controls. The application
        // SQL itself has already passed the AST validator before reaching here.
        c.simple_query("SET NOCOUNT ON; SET XACT_ABORT ON; SET LOCK_TIMEOUT 5000; SET TRANSACTION ISOLATION LEVEL READ COMMITTED;")
            .await?
            .into_results()
            .await?;

        let mut stream = timeout(
            Duration::from_secs(self.config.limits.query_timeout_s),
            c.query(sql, &[]),
        )
        .await
        .context("Timeout iniciando consulta SQL")??;

        // Consume como máximo max_rows + 1 filas. Esto evita cargar un resultset
        // potencialmente enorme en memoria solo para descubrir que estaba truncado.
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.config.limits.query_timeout_s);
        let mut result = Vec::with_capacity(self.config.limits.max_rows);
        let mut truncated = false;
        while result.len() <= self.config.limits.max_rows {
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
            if result.len() == self.config.limits.max_rows {
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

#[async_trait]
impl DatabaseRepository for SqlServer {
    async fn ping(&self) -> Result<(String, String)> {
        SqlServer::ping(self).await
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>> {
        SqlServer::list_tables(self).await
    }

    async fn search_columns(&self, term: &str) -> Result<Vec<ColumnMatch>> {
        SqlServer::search_columns(self, term).await
    }

    async fn describe_table_full(&self, table: &str) -> Result<TableDetail> {
        SqlServer::describe_table_full(self, table).await
    }

    async fn describe_table(&self, table: &str) -> Result<Vec<ColumnInfo>> {
        SqlServer::describe_table(self, table).await
    }

    async fn execute_read(&self, validated: ValidatedSql) -> Result<QueryResult> {
        SqlServer::execute_read(self, validated).await
    }
}

fn tds_config(config: &Config) -> TdsConfig {
    let mut tds = TdsConfig::new();
    tds.host(&config.db.host);
    tds.port(config.db.port);
    tds.database(&config.db.name);
    tds.authentication(AuthMethod::sql_server(&config.db.user, &config.db.password));
    if config.db.trust_cert {
        tds.trust_cert();
    }
    tds
}

fn tcp_connect_timeout(config: &Config) -> Duration {
    Duration::from_secs(if config.llm.connect_timeout_s > 0 {
        config.llm.connect_timeout_s
    } else {
        10
    })
}

fn tls_handshake_timeout(config: &Config) -> Duration {
    Duration::from_secs(if config.limits.query_timeout_s > 0 {
        config.limits.query_timeout_s
    } else {
        20
    })
}

async fn connect_tds(config: &Config) -> Result<TdsClient> {
    let tds = tds_config(config);
    tracing::debug!("🔌 TCP → {}:{}", config.db.host, config.db.port);

    let tcp = timeout(
        tcp_connect_timeout(config),
        TcpStream::connect(tds.get_addr()),
    )
    .await
    .context("Timeout TCP SQL Server")??;

    tcp.set_nodelay(true)?;
    tracing::debug!("✅ TCP conectado");

    let client = timeout(
        tls_handshake_timeout(config),
        Client::connect(tds, tcp.compat_write()),
    )
    .await
    .context("Timeout TLS/autenticación SQL Server")?
    .map_err(|e| anyhow::anyhow!("TLS/autenticación SQL Server: {e}"))?;

    tracing::debug!("✅ Sesión SQL Server establecida");
    Ok(client)
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
        assert!(
            check_privileged_roles(1, 0, 0).is_err(),
            "sysadmin must be denied"
        );
        assert!(
            check_privileged_roles(0, 1, 0).is_err(),
            "db_owner must be denied"
        );
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
        assert_eq!(defaults.limits.query_timeout_s, 30);
        assert_eq!(defaults.llm.connect_timeout_s, 5);
        // SqlServer follows them: TCP <- connect knob, TLS <- query knob.
        assert_eq!(tcp_connect_timeout(&defaults), Duration::from_secs(5));
        assert_eq!(tls_handshake_timeout(&defaults), Duration::from_secs(30));
        // Overrides are respected.
        let over = cfg_with("7", "2");
        assert_eq!(tcp_connect_timeout(&over), Duration::from_secs(2));
        assert_eq!(tls_handshake_timeout(&over), Duration::from_secs(7));
        // Zero falls back to the historical 10s/20s instead of a 0s timeout.
        let fb = cfg_with("0", "0");
        assert_eq!(tcp_connect_timeout(&fb), Duration::from_secs(10));
        assert_eq!(tls_handshake_timeout(&fb), Duration::from_secs(20));
    }
}
