use sql_agent::security::{SecurityPolicy, SqlValidator};

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
fn permite_select() {
    assert!(v().validate("SELECT TOP 10 * FROM dbo.entradaLote").is_ok());
}
#[test]
fn bloquea_delete() {
    assert!(v().validate("DELETE FROM dbo.entradaLote").is_err());
}
#[test]
fn bloquea_update() {
    assert!(v().validate("UPDATE dbo.entradaLote SET x=1").is_err());
}
#[test]
fn bloquea_insert() {
    assert!(v()
        .validate("INSERT INTO dbo.entradaLote VALUES (1)")
        .is_err());
}
#[test]
fn bloquea_drop() {
    assert!(v().validate("DROP TABLE dbo.entradaLote").is_err());
}
#[test]
fn bloquea_exec() {
    assert!(v().validate("EXEC dbo.Test").is_err());
}
#[test]
fn bloquea_multiple_statements() {
    assert!(v()
        .validate("SELECT 1; DELETE FROM dbo.entradaLote")
        .is_err());
}
#[test]
fn bloquea_comentarios() {
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote -- DELETE")
        .is_err());
}
#[test]
fn bloquea_columna_sensible() {
    assert!(v()
        .validate("SELECT password FROM dbo.entradaLote")
        .is_err());
}
#[test]
fn bloquea_tabla_no_autorizada() {
    assert!(v().validate("SELECT * FROM dbo.usuarios_secretos").is_err());
}
#[test]
fn permite_tabla_autorizada() {
    assert!(v().validate("SELECT COUNT(*) FROM dbo.entradaLote").is_ok());
}
#[test]
fn bloquea_system_tables() {
    assert!(v().validate("SELECT * FROM sys.objects").is_err());
}
#[test]
fn permite_cte() {
    assert!(v()
        .validate("WITH x AS (SELECT TOP 10 * FROM dbo.entradaLote) SELECT * FROM x")
        .is_ok());
}
#[test]
fn cte_no_escapa_allowlist() {
    assert!(v()
        .validate("WITH x AS (SELECT * FROM dbo.usuarios_secretos) SELECT * FROM x")
        .is_err());
}
#[test]
fn permite_subquery() {
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote WHERE id IN (SELECT id FROM dbo.entradaLote)")
        .is_ok());
}
#[test]
fn bloquea_demasiados_joins() {
    assert!(v().validate("SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON 1=1 JOIN dbo.entradaLote c ON 1=1 JOIN dbo.entradaLote d ON 1=1 JOIN dbo.entradaLote e ON 1=1 JOIN dbo.entradaLote f ON 1=1 JOIN dbo.entradaLote g ON 1=1").is_err());
}

// ===== PR1 SEC-1: strict read-only gate (strict TDD RED) =====
// Each test below must FAIL before the validator hardening lands
// and PASS after. Positive controls guard valid queries.

#[test]
fn bloquea_select_sin_from() {
    // SELECT without FROM has no allowlist scope to resolve: reject.
    assert!(v().validate("SELECT 1").is_err());
}

#[test]
fn bloquea_star_sin_from() {
    // Bare wildcard with no scope to resolve: reject.
    assert!(v().validate("SELECT *").is_err());
}

#[test]
fn bloquea_where_subquery_no_autorizada() {
    // Unauthorized read hidden in a WHERE subquery must be rejected.
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote WHERE id IN (SELECT id FROM dbo.usuarios_secretos)")
        .is_err());
}

#[test]
fn bloquea_select_into() {
    // SELECT INTO creates a table: it is a write, reject.
    assert!(v()
        .validate("SELECT * INTO dbo.NewT FROM dbo.entradaLote")
        .is_err());
}

#[test]
fn bloquea_union_mixto() {
    // Every UNION branch must be read-only: the second branch hides an
    // unauthorized table inside a WHERE subquery.
    assert!(v()
        .validate("SELECT id FROM dbo.entradaLote UNION ALL SELECT id FROM dbo.entradaLote WHERE id IN (SELECT id FROM dbo.usuarios_secretos)")
        .is_err());
}

// ===== PR1 SEC-1: triangulation (second cases per behavior) =====

#[test]
fn bloquea_where_exists_no_autorizado() {
    // EXISTS is a different subquery form than IN: same gate applies.
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote WHERE EXISTS (SELECT 1 FROM dbo.usuarios_secretos)")
        .is_err());
}

#[test]
fn bloquea_having_subquery_no_autorizada() {
    // HAVING hides the same hole as WHERE: must be walked too.
    assert!(v()
        .validate("SELECT COUNT(*) FROM dbo.entradaLote GROUP BY id HAVING COUNT(*) > (SELECT COUNT(*) FROM dbo.usuarios_secretos)")
        .is_err());
}

#[test]
fn bloquea_join_on_con_subquery_no_autorizada() {
    // JOIN ... ON conditions must be walked like any other expression.
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON EXISTS (SELECT 1 FROM dbo.usuarios_secretos)")
        .is_err());
}

#[test]
fn bloquea_wildcard_calificado_desconocido() {
    // alias.* must resolve to a known FROM/JOIN qualifier.
    assert!(v().validate("SELECT foo.* FROM dbo.entradaLote").is_err());
}

#[test]
fn permite_wildcard_calificado_con_alias() {
    // alias.* over an allowlisted table stays valid.
    assert!(v()
        .validate("SELECT e.* FROM dbo.entradaLote e")
        .is_ok());
}

#[test]
fn permite_union_todo_autorizado() {
    // UNION where every branch is read-only stays valid.
    assert!(v()
        .validate("SELECT id FROM dbo.entradaLote UNION ALL SELECT id FROM dbo.entradaLote")
        .is_ok());
}

