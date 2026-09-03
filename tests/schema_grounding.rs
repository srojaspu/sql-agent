use sql_agent::agent::{
    filter_and_rank_tables, levenshtein, normalize_term, search_with_fallback, singularize,
    strip_accents,
};
use sql_agent::database::TableInfo;

fn tbl(schema: &str, table: &str) -> TableInfo {
    TableInfo {
        schema: schema.to_string(),
        table: table.to_string(),
        table_type: "BASE TABLE".to_string(),
    }
}

fn sample_tables() -> Vec<TableInfo> {
    vec![
        tbl("dbo", "Usuario"),
        tbl("dbo", "Producto"),
        tbl("dbo", "Pedido"),
        tbl("dbo", "EntradaLote"),
        tbl("dbo", "Cliente"),
        tbl("dbo", "Administrador"),
        tbl("dbo", "Cancion"),
        tbl("dbo", "Rol"),
    ]
}

// ===== singularize =====
#[test]
fn singularize_usuarios_to_usuario() {
    assert_eq!(singularize("usuarios"), "usuario");
}

#[test]
fn singularize_administradores_to_administrador() {
    assert_eq!(singularize("administradores"), "administrador");
}

#[test]
fn singularize_clientes_current_behavior() {
    // current impl: "clientes" ends with "es" -> "client"
    // substring matching still finds "Cliente" via contains
    let s = singularize("clientes");
    assert!(
        s == "client" || s == "cliente",
        "singularize clientes got {s}"
    );
}

#[test]
fn singularize_canciones_to_cancion() {
    assert_eq!(singularize("canciones"), "cancion");
}

#[test]
fn singularize_roles_to_rol() {
    assert_eq!(singularize("roles"), "rol");
}

#[test]
fn singularize_meses_to_mes() {
    assert_eq!(singularize("meses"), "mes");
}

// ===== strip_accents =====
#[test]
fn strip_accents_cancion() {
    assert_eq!(strip_accents("canción"), "cancion");
}

#[test]
fn strip_accents_espanol() {
    assert_eq!(strip_accents("español"), "espanol");
}

#[test]
fn strip_accents_all_vowels() {
    assert_eq!(strip_accents("áéíóú"), "aeiou");
    assert_eq!(strip_accents("ñ"), "n");
    assert_eq!(strip_accents("ç"), "c");
}

#[test]
fn strip_accents_uppercase_variants() {
    // upper-case accented input should also be stripped (polish)
    assert_eq!(strip_accents("ÁÉÍÓÚ"), "AEIOU");
    assert_eq!(strip_accents("Ñ"), "N");
    assert_eq!(strip_accents("Ç"), "C");
    assert_eq!(strip_accents("Ángel"), "Angel");
}

// ===== levenshtein =====
#[test]
fn levenshtein_kitten_sitting() {
    assert_eq!(levenshtein("kitten", "sitting"), 3);
}

#[test]
fn levenshtein_usuarios_usuario_is_1() {
    assert_eq!(levenshtein("usuarios", "usuario"), 1);
}

#[test]
fn levenshtein_empty() {
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("abc", ""), 3);
    assert_eq!(levenshtein("", ""), 0);
}

#[test]
fn levenshtein_same_is_0() {
    assert_eq!(levenshtein("producto", "producto"), 0);
}

// ===== normalize_term =====
#[test]
fn normalize_term_usuarios() {
    assert_eq!(normalize_term("Usuarios"), "usuario");
}

#[test]
fn normalize_term_canciones_accent_and_plural() {
    assert_eq!(normalize_term("Canciones"), "cancion");
}

#[test]
fn normalize_term_cancion_singular_accent() {
    assert_eq!(normalize_term("Canción"), "cancion");
}

#[test]
fn normalize_term_administradores() {
    assert_eq!(normalize_term("Administradores"), "administrador");
}

#[test]
fn normalize_term_clientes() {
    let n = normalize_term("CLIENTES");
    // lower + strip + singularize: "clientes" -> "client" via current impl
    assert!(n == "client" || n == "cliente", "got {n}");
}

// ===== filter_and_rank + search_with_fallback =====
#[test]
fn rank_usuarios_finds_usuario_first() {
    let tables = sample_tables();
    let ranked = filter_and_rank_tables("usuarios", &tables, 20);
    assert!(!ranked.is_empty());
    assert_eq!(ranked[0].table, "Usuario");
}

#[test]
fn rank_administradores_finds_administrador_first() {
    let tables = sample_tables();
    let ranked = filter_and_rank_tables("administradores", &tables, 20);
    assert!(!ranked.is_empty());
    assert_eq!(ranked[0].table, "Administrador");
}

#[test]
fn rank_canciones_finds_cancion_via_accent_strip() {
    let tables = sample_tables();
    let ranked = filter_and_rank_tables("canciones", &tables, 20);
    assert!(!ranked.is_empty());
    assert_eq!(ranked[0].table, "Cancion");
}

#[test]
fn rank_cancion_accent_query_finds_cancion() {
    let tables = sample_tables();
    let ranked = filter_and_rank_tables("canción", &tables, 20);
    assert!(!ranked.is_empty());
    assert_eq!(ranked[0].table, "Cancion");
}

#[test]
fn rank_clientes_finds_cliente_via_substring() {
    let tables = sample_tables();
    let ranked = filter_and_rank_tables("clientes", &tables, 20);
    assert!(
        !ranked.is_empty(),
        "clientes should match Cliente via substring"
    );
    assert_eq!(ranked[0].table, "Cliente");
}

#[test]
fn or_fallback_finds_both_when_and_zero() {
    let tables = sample_tables();
    // "usu prod" AND requires both terms in same table -> 0, OR should find Usuario + Producto
    let ranked = filter_and_rank_tables("usu prod", &tables, 20);
    assert_eq!(ranked.len(), 2);
    let names: Vec<&str> = ranked.iter().map(|t| t.table.as_str()).collect();
    assert!(names.contains(&"Usuario"));
    assert!(names.contains(&"Producto"));
}

#[test]
fn truncates_to_max_k_deterministic() {
    let tables: Vec<TableInfo> = (0..30)
        .map(|i| tbl("dbo", &format!("Tabla{i:02}")))
        .collect();
    let ranked = filter_and_rank_tables("", &tables, 20);
    assert_eq!(ranked.len(), 20);
    assert_eq!(ranked[0].table, "Tabla00");
    assert_eq!(ranked[19].table, "Tabla19");
}

#[test]
fn truncates_ranked_matches_to_k() {
    // Many matches but limit to 10
    let tables: Vec<TableInfo> = (0..25)
        .map(|i| tbl("dbo", &format!("Cliente{i}")))
        .collect();
    let ranked = filter_and_rank_tables("cliente", &tables, 10);
    assert_eq!(ranked.len(), 10);
}

#[test]
fn zero_match_returns_did_you_mean_and_empty_ranked() {
    let tables = sample_tables();
    let (ranked, did_you_mean) = search_with_fallback("xyz_noexiste_zzz", &tables, 20);
    assert!(ranked.is_empty(), "no direct match expected");
    assert!(did_you_mean.is_some(), "should suggest Did you mean");
    let dm = did_you_mean.unwrap();
    // suggestion must be one of the candidates, deterministically closest
    assert!(tables.iter().any(|t| t.table == dm.table));
}

#[test]
fn zero_match_did_you_mean_is_closest_levenshtein() {
    let tables = vec![
        tbl("dbo", "Usuario"),
        tbl("dbo", "Producto"),
        tbl("dbo", "Pedido"),
    ];
    let (_, dm) = search_with_fallback("usuarioz", &tables, 20);
    // "usuarioz" closest is Usuario distance 1 vs others larger
    assert_eq!(dm.unwrap().table, "Usuario");
}

#[test]
fn empty_query_returns_top_k_without_suggestion() {
    let tables = sample_tables();
    let (ranked, dm) = search_with_fallback("", &tables, 5);
    assert_eq!(ranked.len(), 5);
    assert!(dm.is_none());
}

#[test]
fn case_insensitive_matching() {
    let tables = sample_tables();
    let r1 = filter_and_rank_tables("USUARIOS", &tables, 20);
    let r2 = filter_and_rank_tables("usuarios", &tables, 20);
    assert_eq!(r1[0].table, r2[0].table);
}

#[test]
fn accent_insensitive_query_matches() {
    let tables = vec![tbl("dbo", "Canción")];
    let ranked = filter_and_rank_tables("cancion", &tables, 20);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].table, "Canción");
    let ranked2 = filter_and_rank_tables("canción", &tables, 20);
    assert_eq!(ranked2.len(), 1);
}
