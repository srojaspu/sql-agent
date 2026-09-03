//! Shared table-name helpers (P2 hygiene).
//!
//! Single canonical implementation for the previously duplicated
//! `normalize_table` (validator/config) and `split_table` /
//! `split_table_name` (agent/sqlserver) helpers. No behavior change:
//! normalization strips brackets and quotes and lowercases (superset of
//! both previous variants); splitting trims, strips brackets/quotes, takes
//! the last two dot-parts, and defaults a missing schema to `dbo`.

/// Normalize a table identifier for allowlist comparison:
/// strip `[`, `]` and `"` then lowercase.
pub fn normalize_table_name(s: &str) -> String {
    s.replace(['[', ']', '"'], "").to_ascii_lowercase()
}

/// Split `schema.table` (or `[schema].[table]`, `"schema"."table"`) into
/// `(schema, table)`. Takes the last two dot-parts so `db.schema.table`
/// resolves to `(schema, table)`; a bare name defaults to `dbo`.
/// Trims surrounding whitespace first.
pub fn split_table_name(s: &str) -> (String, String) {
    let cleaned = s.trim().replace(['[', ']', '"'], "");
    let parts: Vec<&str> = cleaned.split('.').collect();
    if parts.len() >= 2 {
        (
            parts[parts.len() - 2].to_owned(),
            parts[parts.len() - 1].to_owned(),
        )
    } else {
        ("dbo".to_owned(), cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_brackets_quotes_and_lowercases() {
        assert_eq!(normalize_table_name("dbo.Foo"), "dbo.foo");
        assert_eq!(normalize_table_name("[dbo].[Foo]"), "dbo.foo");
        assert_eq!(normalize_table_name("\"dbo\".\"Foo\""), "dbo.foo");
        assert_eq!(normalize_table_name("DBO.ENTRADALOTE"), "dbo.entradalote");
    }

    #[test]
    fn normalize_matches_both_legacy_variants() {
        // Legacy config variant stripped only brackets; legacy validator
        // variant also stripped quotes. The shared helper is the superset:
        // identical for bracket-only input, quote-stripping for quoted input.
        assert_eq!(normalize_table_name("[dbo].[a]"), "dbo.a");
        assert_eq!(
            normalize_table_name("[dbo].[a]"),
            "[dbo].[a]".replace(['[', ']'], "").to_ascii_lowercase()
        );
        assert_eq!(normalize_table_name("\"dbo\".\"a\""), "dbo.a");
    }

    #[test]
    fn split_with_schema_keeps_schema() {
        assert_eq!(
            split_table_name("dbo.Usuario"),
            ("dbo".into(), "Usuario".into())
        );
        assert_eq!(
            split_table_name("[dbo].[Usuario]"),
            ("dbo".into(), "Usuario".into())
        );
        assert_eq!(
            split_table_name("\"dbo\".\"Usuario\""),
            ("dbo".into(), "Usuario".into())
        );
    }

    #[test]
    fn split_bare_name_defaults_to_dbo() {
        assert_eq!(
            split_table_name("usuarios"),
            ("dbo".into(), "usuarios".into())
        );
        assert_eq!(
            split_table_name("  usuarios  "),
            ("dbo".into(), "usuarios".into())
        );
    }

    #[test]
    fn split_takes_last_two_parts() {
        assert_eq!(
            split_table_name("db.dbo.Usuario"),
            ("dbo".into(), "Usuario".into())
        );
    }
}
