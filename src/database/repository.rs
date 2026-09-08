//! Database repository seam: every DB call behind one trait.
//!
//! [`DatabaseRepository`] mirrors the [`SqlServer`](super::sqlserver::SqlServer)
//! surface the agent needs: connectivity plus schema discovery plus the single
//! validated-read path. The critical invariant lives on
//! [`DatabaseRepository::execute_read`]: it takes a
//! [`ValidatedSql`](crate::security::ValidatedSql) **by value**, never `&str`
//! or [`String`], so only validator-approved SQL can reach the driver.
//!
//! Production code implements this for [`SqlServer`](super::sqlserver::SqlServer)
//! (single signature change inside `execute_read`: `validated.as_str()`).
//! Tests implement it with a fake that records the [`ValidatedSql`] it
//! receives and returns a canned [`QueryResult`](super::sqlserver::QueryResult).

use async_trait::async_trait;

use crate::database::sqlserver::QueryResult;
use crate::database::{
    schema::{ColumnMatch, TableDetail},
    ColumnInfo, TableInfo,
};
use crate::security::ValidatedSql;

/// Agent-facing database surface (object-safe, `Send + Sync` for `Arc<dyn>`).
#[async_trait]
pub trait DatabaseRepository: Send + Sync {
    /// Verify connectivity and the read-only gate (fail-closed).
    async fn ping(&self) -> anyhow::Result<(String, String)>;

    /// List every visible table and view with its type.
    async fn list_tables(&self) -> anyhow::Result<Vec<TableInfo>>;

    /// Search column names across the whole schema (LIKE-escaped).
    async fn search_columns(&self, term: &str) -> anyhow::Result<Vec<ColumnMatch>>;

    /// Enriched describe: columns + PK/FK + view definition + TOP 5 + COUNT.
    async fn describe_table_full(&self, table: &str) -> anyhow::Result<TableDetail>;

    /// Column list for one table (shared projection with the full describe).
    async fn describe_table(&self, table: &str) -> anyhow::Result<Vec<ColumnInfo>>;

    /// Execute validator-approved SQL (single call site: `execute_read_tool`).
    ///
    /// Takes [`ValidatedSql`] by value: the caller must have validated first,
    /// and the approved text cannot be reused for a second unvalidated query
    /// without an explicit clone.
    async fn execute_read(&self, validated: ValidatedSql) -> anyhow::Result<QueryResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Probe {
        received: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DatabaseRepository for Probe {
        async fn ping(&self) -> anyhow::Result<(String, String)> {
            Ok(("db".into(), "login".into()))
        }

        async fn list_tables(&self) -> anyhow::Result<Vec<TableInfo>> {
            Ok(vec![])
        }

        async fn search_columns(&self, _term: &str) -> anyhow::Result<Vec<ColumnMatch>> {
            Ok(vec![])
        }

        async fn describe_table_full(&self, _table: &str) -> anyhow::Result<TableDetail> {
            Ok(TableDetail {
                columns: vec![],
                primary_keys: vec![],
                foreign_keys: vec![],
                view_definition: None,
                sample_rows: vec![],
                row_count: 0,
            })
        }

        async fn describe_table(&self, _table: &str) -> anyhow::Result<Vec<ColumnInfo>> {
            Ok(vec![])
        }

        async fn execute_read(&self, validated: ValidatedSql) -> anyhow::Result<QueryResult> {
            self.received
                .lock()
                .unwrap()
                .push(validated.as_str().to_owned());
            Ok(QueryResult {
                row_count: 0,
                truncated: false,
                rows: vec![],
            })
        }
    }

    #[tokio::test]
    async fn trait_accepts_only_validated_sql_by_value() {
        use crate::security::{SecurityPolicy, SqlValidator};
        let v = SqlValidator::new(SecurityPolicy {
            max_sql_length: 10_000,
            allowed_tables: vec!["dbo.entradaLote".into()],
            block_sensitive_columns: true,
            block_comments: true,
            allow_cte: true,
            allow_system_tables: false,
            max_joins: 5,
            max_subqueries: 5,
        });
        let validated = v
            .validate("SELECT TOP 10 * FROM dbo.entradaLote")
            .expect("must validate");
        let probe = Probe {
            received: Mutex::new(Vec::new()),
        };
        probe.execute_read(validated).await.expect("must run");
        assert_eq!(
            probe.received.lock().unwrap().as_slice(),
            ["SELECT TOP 10 * FROM dbo.entradaLote"]
        );
    }
}
