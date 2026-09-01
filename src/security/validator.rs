use anyhow::{bail, Result};
use sqlparser::{
    ast::{Query, Select, SetExpr, Statement, TableFactor, TableWithJoins},
    dialect::MsSqlDialect,
    parser::Parser,
};
use std::collections::HashSet;

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
    policy: SecurityPolicy,
}

impl SqlValidator {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self { policy }
    }

    pub fn validate(&self, sql: &str) -> Result<()> {
        let sql = sql.trim();
        if sql.is_empty() {
            bail!("SQL vacío");
        }
        if sql.len() > self.policy.max_sql_length {
            bail!("SQL supera MAX_SQL_LENGTH");
        }
        if self.policy.block_comments
            && (sql.contains("--") || sql.contains("/*") || sql.contains("*/"))
        {
            bail!("Comentarios SQL no permitidos");
        }

        let statements = Parser::parse_sql(&MsSqlDialect {}, sql)
            .map_err(|e| anyhow::anyhow!("SQL inválido: {e}"))?;
        if statements.len() != 1 {
            bail!("Solo se permite un statement");
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
                bail!("Operación/función bloqueada: {word}");
            }
        }

        let statement = &statements[0];
        let query = match statement {
            Statement::Query(q) => q,
            _ => bail!("Solo se permite SELECT/CTE SELECT"),
        };

        let mut ctx = ValidationContext::default();
        self.validate_query(query, &mut ctx)?;

        if ctx.joins > self.policy.max_joins {
            bail!("Demasiados JOINs: máximo {}", self.policy.max_joins);
        }
        if ctx.subqueries > self.policy.max_subqueries {
            bail!(
                "Demasiadas subconsultas: máximo {}",
                self.policy.max_subqueries
            );
        }

        if !self.policy.allow_system_tables {
            for table in &ctx.tables {
                let n = normalize_table(table);
                if n.starts_with("sys.")
                    || n.contains("information_schema")
                    || n.starts_with("master.")
                {
                    bail!("Acceso a metadatos/sistema no permitido: {table}");
                }
            }
        }

        if !self.policy.allowed_tables.is_empty() {
            let allowed: HashSet<String> = self
                .policy
                .allowed_tables
                .iter()
                .map(|s| normalize_table(s))
                .collect();
            for table in &ctx.tables {
                if ctx.ctes.contains(&normalize_table(table)) {
                    continue;
                }
                if !allowed.contains(&normalize_table(table)) {
                    bail!("Tabla no permitida: {table}");
                }
            }
        }

        if self.policy.block_sensitive_columns {
            for word in [
                "PASSWORD",
                "PASSWD",
                "SECRET",
                "TOKEN",
                "ACCESS_TOKEN",
                "REFRESH_TOKEN",
                "API_KEY",
                "PRIVATE_KEY",
                "CLIENT_SECRET",
            ] {
                if contains_word(&sql.to_ascii_uppercase(), word) {
                    bail!("Columna sensible bloqueada: {word}");
                }
            }
        }

        Ok(())
    }

    fn validate_query(&self, query: &Query, ctx: &mut ValidationContext) -> Result<()> {
        if let Some(with) = &query.with {
            if !self.policy.allow_cte {
                bail!("CTE/WITH no permitido");
            }
            for cte in &with.cte_tables {
                ctx.ctes.insert(normalize_table(&cte.alias.name.value));
                self.validate_query(&cte.query, ctx)?;
            }
        }
        self.validate_set_expr(&query.body, ctx)
    }

    fn validate_set_expr(&self, expr: &SetExpr, ctx: &mut ValidationContext) -> Result<()> {
        match expr {
            SetExpr::Select(select) => self.validate_select(select, ctx),
            SetExpr::SetOperation { left, right, .. } => {
                self.validate_set_expr(left, ctx)?;
                self.validate_set_expr(right, ctx)
            }
            SetExpr::Query(query) => {
                ctx.subqueries += 1;
                self.validate_query(query, ctx)
            }
            _ => bail!("Expresión SQL no permitida"),
        }
    }

    fn validate_select(&self, select: &Select, ctx: &mut ValidationContext) -> Result<()> {
        for twj in &select.from {
            self.validate_table_with_joins(twj, ctx)?;
        }
        Ok(())
    }

    fn validate_table_with_joins(
        &self,
        twj: &TableWithJoins,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
        self.validate_table_factor(&twj.relation, ctx)?;
        ctx.joins += twj.joins.len();
        for join in &twj.joins {
            self.validate_table_factor(&join.relation, ctx)?;
        }
        Ok(())
    }

    fn validate_table_factor(
        &self,
        factor: &TableFactor,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
        match factor {
            TableFactor::Table { name, .. } => {
                if name.0.len() > 2 {
                    bail!("Referencias de servidor/base de datos no permitidas: {name}");
                }
                ctx.tables.insert(name.to_string());
            }
            TableFactor::Derived { subquery, .. } => {
                ctx.subqueries += 1;
                self.validate_query(subquery, ctx)?;
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.validate_table_with_joins(table_with_joins, ctx)?;
            }
            TableFactor::TableFunction { .. } => {
                bail!("Funciones de tabla no permitidas");
            }
            _ => {
                bail!("Tipo de tabla no permitido");
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct ValidationContext {
    tables: HashSet<String>,
    ctes: HashSet<String>,
    joins: usize,
    subqueries: usize,
}

fn normalize_table(s: &str) -> String {
    s.replace('[', "")
        .replace(']', "")
        .replace('"', "")
        .to_ascii_lowercase()
}

fn contains_word(sql: &str, word: &str) -> bool {
    sql.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|p| p.eq_ignore_ascii_case(word))
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
}
