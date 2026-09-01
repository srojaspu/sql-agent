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
