//! Bounded describe helpers: COUNT cap, safe projection, sample SQL.
//!
//! Pure move from `database::sqlserver` (slice E, step 2): no query text,
//! timeout value, SET option, or permission check changes. Covers
//! `describe_count_sql` + `resolve_describe_row_count` + `safe_columns` +
//! `describe_sample_sql` + `DESCRIBE_COUNT_CAP`.

use anyhow::Result;

use super::queries::escape_ident;
use crate::database::ColumnInfo;
use crate::security::is_sensitive_column;

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

/// Filter describe columns to non-sensitive names only, reusing the
/// validator sensitive list as the single source. Returns safe names.
pub fn safe_columns(columns: &[ColumnInfo]) -> Vec<String> {
    columns
        .iter()
        .filter(|c| !is_sensitive_column(&c.column))
        .map(|c| c.column.clone())
        .collect()
}

/// Build the safe TOP 5 sample query over already-escaped qualified name.
/// Returns None when no column is safe: the caller must skip the fetch and
/// return an empty sample instead of leaking values.
pub fn describe_sample_sql(qualified: &str, safe_cols: &[String]) -> Option<String> {
    if safe_cols.is_empty() {
        return None;
    }
    let cols = safe_cols
        .iter()
        .map(|c| escape_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("SELECT TOP 5 {cols} FROM {qualified}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ===== slice-1a: safe projection (RED) =====

    fn sample_columns_for_safe_test() -> Vec<crate::database::ColumnInfo> {
        vec![
            crate::database::ColumnInfo {
                column: "id".into(),
                data_type: "int".into(),
                nullable: false,
                ordinal: 1,
            },
            crate::database::ColumnInfo {
                column: "password".into(),
                data_type: "nvarchar".into(),
                nullable: true,
                ordinal: 2,
            },
            crate::database::ColumnInfo {
                column: "name".into(),
                data_type: "nvarchar".into(),
                nullable: true,
                ordinal: 3,
            },
        ]
    }

    #[test]
    fn safe_columns_keeps_only_non_sensitive() {
        // Mixed table: only safe columns survive (production code must run).
        let cols = sample_columns_for_safe_test();
        let safe = safe_columns(&cols);
        assert_eq!(safe, vec!["id".to_string(), "name".to_string()]);
        assert!(
            !safe.iter().any(|c| c.eq_ignore_ascii_case("password")),
            "sensitive column must be filtered, got: {safe:?}"
        );
    }

    #[test]
    fn describe_sample_sql_projects_safe_cols_only() {
        // Triangulation: safe projection builds TOP 5 over safe cols (not *).
        let sql = describe_sample_sql("[dbo].[Users]", &["id".to_string()])
            .expect("mixed table must produce SQL");
        assert!(sql.contains("TOP 5"), "sample must be TOP 5, got: {sql}");
        assert!(sql.contains("[id]"), "safe col must appear, got: {sql}");
        assert!(
            !sql.contains("password"),
            "sensitive col must never leave DB, got: {sql}"
        );
        assert!(!sql.contains('*'), "must not use SELECT *, got: {sql}");
    }

    #[test]
    fn describe_sample_sql_none_when_all_sensitive() {
        // All-sensitive table: None means "fetch nothing" (empty sample).
        assert_eq!(describe_sample_sql("[dbo].[Secrets]", &[]), None);
        // Non-empty safe list must still produce a query (companion case).
        let some = describe_sample_sql("[dbo].[Users]", &["id".to_string()]);
        assert!(some.is_some(), "non-empty safe list must produce SQL");
    }
}
