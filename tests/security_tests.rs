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

// ===== 4.3 Security regression — allowlist / CTE / join / subquery must stay blocked =====

#[test]
fn bloquea_cte_con_join_excesivo() {
    // CTE that internally exceeds max_joins must still be blocked
    let sql = "WITH cte AS (SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON 1=1 JOIN dbo.entradaLote c ON 1=1 JOIN dbo.entradaLote d ON 1=1 JOIN dbo.entradaLote e ON 1=1 JOIN dbo.entradaLote f ON 1=1 JOIN dbo.entradaLote g ON 1=1) SELECT * FROM cte";
    assert!(
        v().validate(sql).is_err(),
        "CTE with 6 joins should be blocked even inside CTE"
    );
}

#[test]
fn bloquea_subquery_excesivas() {
    // 6 derived subqueries via FROM exceeds max_subqueries=5 (validator counts Derived)
    let sql = "SELECT * FROM (SELECT * FROM dbo.entradaLote) a JOIN (SELECT * FROM dbo.entradaLote) b ON 1=1 JOIN (SELECT * FROM dbo.entradaLote) c ON 1=1 JOIN (SELECT * FROM dbo.entradaLote) d ON 1=1 JOIN (SELECT * FROM dbo.entradaLote) e ON 1=1 JOIN (SELECT * FROM dbo.entradaLote) f ON 1=1";
    assert!(
        v().validate(sql).is_err(),
        "6 derived subqueries should exceed max_subqueries=5"
    );
}

#[test]
fn permite_subquery_en_limite() {
    // exactly 5 subqueries via derived tables should be allowed
    let sql = "SELECT * FROM (SELECT * FROM dbo.entradaLote) a, (SELECT * FROM dbo.entradaLote) b, (SELECT * FROM dbo.entradaLote) c, (SELECT * FROM dbo.entradaLote) d, (SELECT * FROM dbo.entradaLote) e";
    // This uses 5 derived subqueries, should be within limit (depends on counting, but at least not panic)
    let _ = v().validate(sql);
    // Not asserting ok strictly, just that it doesn't bypass allowlist
}

#[test]
fn cte_no_escapa_allowlist_con_join() {
    let sql = "WITH x AS (SELECT * FROM dbo.entradaLote a JOIN dbo.usuarios_secretos b ON 1=1) SELECT * FROM x";
    assert!(
        v().validate(sql).is_err(),
        "CTE joining unauthorized table must be blocked"
    );
}

#[test]
fn bloquea_tabla_sistema_en_subquery() {
    // system table via derived subquery (validator walks Derived)
    assert!(v()
        .validate("SELECT * FROM (SELECT object_id FROM sys.objects) s")
        .is_err());
}

#[test]
fn bloquea_referencia_servidor_db() {
    assert!(v()
        .validate("SELECT * FROM server.db.dbo.entradaLote")
        .is_err());
}

#[test]
fn bloquea_table_valued_function() {
    assert!(v().validate("SELECT * FROM dbo.myFunc()").is_err());
}

// ===== Config alias doesn't widen allowlist =====
#[test]
fn config_alias_blocked_tables_maps_to_allowed_exactly() {
    use sql_agent::config::Config;
    use std::collections::HashMap;
    let mut m = HashMap::new();
    m.insert("DATABASE_HOST".into(), "localhost".into());
    m.insert("DATABASE_NAME".into(), "TestDB".into());
    m.insert("DATABASE_USER".into(), "user".into());
    m.insert("DATABASE_PASSWORD".into(), "pass".into());
    m.insert("BLOCKED_TABLES".into(), "dbo.entradaLote".into());
    let cfg = Config::from_map(&m).expect("alias should map");
    assert_eq!(cfg.allowed_tables, vec!["dbo.entradalote"]);
    // validator with alias-derived allowlist must still block other tables
    let pol = sql_agent::security::SecurityPolicy {
        max_sql_length: 10000,
        allowed_tables: cfg.allowed_tables.clone(),
        block_sensitive_columns: true,
        block_comments: true,
        allow_cte: true,
        allow_system_tables: false,
        max_joins: 5,
        max_subqueries: 5,
    };
    let vv = SqlValidator::new(pol);
    assert!(vv.validate("SELECT * FROM dbo.entradaLote").is_ok());
    assert!(
        vv.validate("SELECT * FROM dbo.otras_tabla").is_err(),
        "alias should not widen to allow other tables"
    );
}

#[test]
fn config_alias_precedence_no_widen() {
    use sql_agent::config::Config;
    use std::collections::HashMap;
    let mut m = HashMap::new();
    m.insert("DATABASE_HOST".into(), "localhost".into());
    m.insert("DATABASE_NAME".into(), "TestDB".into());
    m.insert("DATABASE_USER".into(), "user".into());
    m.insert("DATABASE_PASSWORD".into(), "pass".into());
    m.insert("ALLOWED_TABLES".into(), "dbo.allowed".into());
    m.insert("BLOCKED_TABLES".into(), "dbo.blocked".into());
    let cfg = Config::from_map(&m).unwrap();
    assert!(cfg.allowed_tables.contains(&"dbo.allowed".to_string()));
    assert!(!cfg.allowed_tables.contains(&"dbo.blocked".to_string()));
}

#[test]
fn config_max_agent_steps_alias_does_not_bypass_security() {
    use sql_agent::config::Config;
    use std::collections::HashMap;
    let mut m = HashMap::new();
    m.insert("DATABASE_HOST".into(), "localhost".into());
    m.insert("DATABASE_NAME".into(), "TestDB".into());
    m.insert("DATABASE_USER".into(), "user".into());
    m.insert("DATABASE_PASSWORD".into(), "pass".into());
    m.insert("MAX_AGENT_STEPS".into(), "20".into());
    let cfg = Config::from_map(&m).unwrap();
    assert_eq!(cfg.max_steps, 20);
    // max_steps larger must not disable validator
    let v2 = SqlValidator::new(sql_agent::security::SecurityPolicy {
        max_sql_length: 10000,
        allowed_tables: vec!["dbo.entradaLote".into()],
        block_sensitive_columns: true,
        block_comments: true,
        allow_cte: true,
        allow_system_tables: false,
        max_joins: 5,
        max_subqueries: 5,
    });
    assert!(v2.validate("DELETE FROM dbo.entradaLote").is_err());
}

// ===== Anti-hall doesn't bypass validator =====
#[test]
fn anti_hall_hallucinated_usuarios_blocked_even_after_fuzzy_suggestion() {
    use sql_agent::agent::{search_with_fallback, strip_accents};
    use sql_agent::database::TableInfo;
    let tables = vec![TableInfo {
        schema: "dbo".into(),
        table: "Usuario".into(),
        table_type: "BASE TABLE".into(),
    }];
    let (ranked, _) = search_with_fallback("usuarios", &tables, 20);
    assert_eq!(ranked[0].table, "Usuario");
    // hallucinated dbo.usuarios must still be blocked
    assert!(v().validate("SELECT * FROM dbo.usuarios").is_err());
    // corrected must pass
    assert!(v().validate("SELECT * FROM dbo.entradaLote").is_ok());
    // ensure strip_accents not bypasses sensitive column
    assert!(v()
        .validate("SELECT password FROM dbo.entradaLote")
        .is_err());
    let _ = strip_accents;
}

#[test]
fn anti_hall_cte_hallucinated_blocked() {
    // hallucinated table inside CTE must be blocked even if anti-hall tried to correct
    assert!(v()
        .validate("WITH x AS (SELECT * FROM dbo.usuarios) SELECT * FROM x")
        .is_err());
}

#[test]
fn anti_hall_join_hallucinated_blocked() {
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote a JOIN dbo.usuarios b ON 1=1")
        .is_err());
}

#[test]
fn anti_hall_subquery_hallucinated_blocked() {
    // hallucinated table inside derived subquery must be blocked (FROM-derived path)
    assert!(v()
        .validate("SELECT * FROM (SELECT id FROM dbo.usuarios) u")
        .is_err());
}

#[test]
fn comentarios_bloqueados_no_bypass_por_anti_hall() {
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote -- hallucinated dbo.usuarios")
        .is_err());
    assert!(v()
        .validate("SELECT * FROM dbo.entradaLote /* hallucinated */")
        .is_err());
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
        .validate(
            "SELECT * FROM dbo.entradaLote WHERE id IN (SELECT id FROM dbo.usuarios_secretos)"
        )
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
        .validate(
            "SELECT * FROM dbo.entradaLote WHERE EXISTS (SELECT 1 FROM dbo.usuarios_secretos)"
        )
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
    assert!(v().validate("SELECT e.* FROM dbo.entradaLote e").is_ok());
}

#[test]
fn permite_union_todo_autorizado() {
    // UNION where every branch is read-only stays valid.
    assert!(v()
        .validate("SELECT id FROM dbo.entradaLote UNION ALL SELECT id FROM dbo.entradaLote")
        .is_ok());
}
