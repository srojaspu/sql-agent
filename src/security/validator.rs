//! Facade validator: policy plus the single `validate` entry point.
//!
//! Slice E, step 2 (pure move): identifier collection lives in
//! [`super::identifiers`], expression/function checks in [`super::expr`],
//! query/table checks plus [`ValidationContext`](super::tables::ValidationContext)
//! in [`super::tables`], wildcard scope in [`super::scope`]. No rule,
//! message, limit, or ordering change here.

use crate::error::ValidationBlocked;
use crate::security::ValidatedSql;

/// Local `bail!` equivalent producing `ValidationBlocked` with identical messages.
/// Every call site keeps its original format string verbatim; only the error
/// type changes from `anyhow::Error` to `ValidationBlocked::Blocked`.
macro_rules! blocked {
    ($($arg:tt)*) => {
        return Err(ValidationBlocked::Blocked(format!($($arg)*)))
    };
}
use sqlparser::{dialect::MsSqlDialect, parser::Parser, ast::Statement};

use super::expr::contains_word;
use super::identifiers::{collect_identifiers, is_sensitive_column};
use super::tables::ValidationContext;
use crate::util::normalize_table_name;

#[derive(Clone, Debug)]
pub struct SecurityPolicy {
    pub max_sql_length: usize,
    pub allowed_tables: Vec<String>,
    pub block_sensitive_columns: bool,
    pub block_comments: bool,
    pub allow_cte: bool,
    pub allow_system_tables: bool,
    pub max_joins: usize,
    pub max_subqueries: usize,
}

pub struct SqlValidator {
    pub(crate) policy: SecurityPolicy,
}

impl SqlValidator {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self { policy }
    }

    pub fn validate(&self, sql: &str) -> Result<ValidatedSql, ValidationBlocked> {
        let sql = sql.trim();
        if sql.is_empty() {
            blocked!("SQL vacío");
        }
        if sql.len() > self.policy.max_sql_length {
            blocked!("SQL supera MAX_SQL_LENGTH");
        }
        if self.policy.block_comments
            && (sql.contains("--") || sql.contains("/*") || sql.contains("*/"))
        {
            blocked!("Comentarios SQL no permitidos");
        }

        let statements = Parser::parse_sql(&MsSqlDialect {}, sql)
            .map_err(|e| ValidationBlocked::Blocked(format!("SQL inválido: {e}")))?;
        if statements.len() != 1 {
            blocked!("Solo se permite un statement");
        }

        let upper = sql.to_ascii_uppercase();
        for word in [
            "XP_CMDSHELL",
            "SP_EXECUTESQL",
            "OPENROWSET",
            "OPENQUERY",
            "OPENDATASOURCE",
            "BULK",
            "BACKUP",
            "RESTORE",
            "DBCC",
            "WAITFOR",
            "SHUTDOWN",
            "GRANT",
            "REVOKE",
            "DENY",
            "KILL",
        ] {
            if contains_word(&upper, word) {
                blocked!("Operación/función bloqueada: {word}");
            }
        }

        let statement = &statements[0];
        let query = match statement {
            Statement::Query(q) => q,
            _ => blocked!("Solo se permite SELECT/CTE SELECT"),
        };

        let mut ctx = ValidationContext {
            allowed: if self.policy.allowed_tables.is_empty() {
                None
            } else {
                // Build the allowlist once per validation and share it with every
                // scope check below. `None` means "no allowlist configured".
                Some(
                    self.policy
                        .allowed_tables
                        .iter()
                        .map(|s| normalize_table_name(s))
                        .collect(),
                )
            },
            ..ValidationContext::default()
        };
        self.validate_query(query, &mut ctx)?;

        if ctx.joins > self.policy.max_joins {
            blocked!("Demasiados JOINs: máximo {}", self.policy.max_joins);
        }
        if ctx.subqueries > self.policy.max_subqueries {
            blocked!(
                "Demasiadas subconsultas: máximo {}",
                self.policy.max_subqueries
            );
        }

        if !self.policy.allow_system_tables {
            for table in &ctx.tables {
                let n = normalize_table_name(table);
                if n.starts_with("sys.")
                    || n.contains("information_schema")
                    || n.starts_with("master.")
                {
                    blocked!("Acceso a metadatos/sistema no permitido: {table}");
                }
            }
        }

        if let Some(allowed) = &ctx.allowed {
            for table in &ctx.tables {
                if ctx.ctes.contains(&normalize_table_name(table)) {
                    continue;
                }
                if !allowed.contains(&normalize_table_name(table)) {
                    blocked!("Tabla no permitida: {table}");
                }
            }
        }

        if self.policy.block_sensitive_columns {
            // AST identifiers only: literals and comments never enter
            // `collect_identifiers`, so `'password reset'` passes while
            // `my_token` (MY + TOKEN boundary) is blocked.
            for ident in collect_identifiers(&statements[0]) {
                if is_sensitive_column(&ident) {
                    blocked!("Columna sensible bloqueada: {ident}");
                }
            }
        }

        Ok(ValidatedSql::from_trusted(sql.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v() -> SqlValidator {
        SqlValidator::new(SecurityPolicy {
            max_sql_length: 10_000,
            allowed_tables: vec!["dbo.entradaLote".into()],
            block_sensitive_columns: true,
            block_comments: true,
            allow_cte: true,
            allow_system_tables: false,
            max_joins: 5,
            max_subqueries: 5,
        })
    }
    #[test]
    fn select_ok() {
        assert!(v().validate("SELECT TOP 10 * FROM dbo.entradaLote").is_ok());
    }
    #[test]
    fn max_joins_boundary_allowed() {
        let sql = "SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON 1=1 JOIN dbo.entradaLote c ON 1=1 JOIN dbo.entradaLote d ON 1=1 JOIN dbo.entradaLote e ON 1=1 JOIN dbo.entradaLote f ON 1=1";
        assert!(v().validate(sql).is_ok());
    }
    #[test]
    fn max_joins_exceeded() {
        let sql = "SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON 1=1 JOIN dbo.entradaLote c ON 1=1 JOIN dbo.entradaLote d ON 1=1 JOIN dbo.entradaLote e ON 1=1 JOIN dbo.entradaLote f ON 1=1 JOIN dbo.entradaLote g ON 1=1";
        assert!(v().validate(sql).is_err());
    }
    #[test]
    fn delete_blocked() {
        assert!(v().validate("DELETE FROM dbo.entradaLote").is_err());
    }
    #[test]
    fn update_blocked() {
        assert!(v().validate("UPDATE dbo.entradaLote SET x=1").is_err());
    }
    #[test]
    fn insert_blocked() {
        assert!(v()
            .validate("INSERT INTO dbo.entradaLote VALUES (1)")
            .is_err());
    }
    #[test]
    fn drop_blocked() {
        assert!(v().validate("DROP TABLE dbo.entradaLote").is_err());
    }
    #[test]
    fn multi_statement_blocked() {
        assert!(v().validate("SELECT 1; SELECT 2").is_err());
    }
    #[test]
    fn comments_blocked() {
        assert!(v().validate("SELECT 1 -- DELETE").is_err());
    }
    #[test]
    fn sensitive_column_blocked() {
        assert!(v()
            .validate("SELECT password FROM dbo.entradaLote")
            .is_err());
    }
    #[test]
    fn unauthorized_table_blocked() {
        assert!(v().validate("SELECT * FROM dbo.usuarios_secretos").is_err());
    }
    #[test]
    fn cte_allowed() {
        assert!(v()
            .validate("WITH x AS (SELECT TOP 10 * FROM dbo.entradaLote) SELECT * FROM x")
            .is_ok());
    }
    #[test]
    fn cte_cannot_escape_allowlist() {
        assert!(v()
            .validate("WITH x AS (SELECT * FROM dbo.usuarios_secretos) SELECT * FROM x")
            .is_err());
    }
    #[test]
    fn subquery_allowlist() {
        assert!(v()
            .validate("SELECT * FROM dbo.entradaLote WHERE id IN (SELECT id FROM dbo.entradaLote)")
            .is_ok());
    }
    #[test]
    fn system_blocked() {
        assert!(v().validate("SELECT * FROM sys.objects").is_err());
    }
    #[test]
    fn system_func_via_from_blocked() {
        assert!(v()
            .validate("SELECT SUSER_SNAME() FROM dbo.entradaLote")
            .is_err());
        assert!(v()
            .validate("SELECT HOST_NAME() FROM dbo.entradaLote")
            .is_err());
        assert!(v()
            .validate("SELECT DB_NAME() FROM dbo.entradaLote")
            .is_err());
        assert!(v()
            .validate("SELECT GETDATE() FROM dbo.entradaLote")
            .is_err());
        assert!(v()
            .validate("SELECT @@VERSION FROM dbo.entradaLote")
            .is_err());
    }
    #[test]
    fn normal_select_with_allowed_func_passes() {
        assert!(v().validate("SELECT COUNT(*) FROM dbo.entradaLote").is_ok());
    }
    #[test]
    fn shared_allowlist_same_allow_block_for_scope_and_global() {
        // The prebuilt allowlist must give identical verdicts through the
        // per-scope wildcard gate and the global table gate.
        assert!(v().validate("SELECT * FROM dbo.entradaLote").is_ok());
        assert!(v().validate("SELECT e.* FROM dbo.entradaLote e").is_ok());
        assert!(v().validate("SELECT * FROM dbo.usuarios_secretos").is_err());
        assert!(v()
            .validate("SELECT u.* FROM dbo.usuarios_secretos u")
            .is_err());
    }
    #[test]
    fn deep_derived_nesting_fails_fast_on_budget() {
        // 10 nested derived tables with max_subqueries=5 must bail on budget
        // (early Derived check), not slip through or recurse needlessly deep.
        // Kept at 10 (not deeper) so the parser itself stays within stack.
        let mut sql = "SELECT * FROM dbo.entradaLote".to_string();
        for i in 0..10 {
            sql = format!("SELECT * FROM ({sql}) AS d{i}");
        }
        let err = v().validate(&sql).unwrap_err().to_string();
        assert!(
            err.contains("subconsultas"),
            "deep nesting must fail on subquery budget, got: {err}"
        );
    }
}
