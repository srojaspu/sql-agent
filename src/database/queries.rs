//! SQL text plus T-SQL escaping helpers.
//!
//! Pure move from `database::sqlserver` (slice E, step 2): no query text,
//! timeout value, SET option, or permission check changes. Covers the
//! INFORMATION_SCHEMA projections plus LIKE/identifier escaping.

/// Query listing every visible table AND view with its type.
pub(crate) fn list_tables_query() -> &'static str {
    "SELECT TABLE_SCHEMA,TABLE_NAME,TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES \
     ORDER BY TABLE_SCHEMA,TABLE_NAME"
}

/// Parameterized column search; the caller passes a pre-escaped LIKE pattern as @P1.
pub(crate) fn search_columns_query() -> &'static str {
    "SELECT c.TABLE_SCHEMA,c.TABLE_NAME,c.COLUMN_NAME,c.DATA_TYPE \
     FROM INFORMATION_SCHEMA.COLUMNS c \
     WHERE c.COLUMN_NAME LIKE @P1 \
     ORDER BY c.TABLE_SCHEMA,c.TABLE_NAME,c.ORDINAL_POSITION"
}

/// Escape T-SQL LIKE wildcards so the term matches literally.
/// Bracket-escaping needs no ESCAPE clause: % -> [%], _ -> [_], [ -> [[], ] -> []].
pub(crate) fn escape_like_pattern(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    for ch in term.chars() {
        match ch {
            '%' => out.push_str("[%]"),
            '_' => out.push_str("[_]"),
            '[' => out.push_str("[[]"),
            ']' => out.push_str("[]]"),
            _ => out.push(ch),
        }
    }
    out
}

/// Quote a T-SQL identifier by wrapping in brackets, doubling any closing bracket.
pub(crate) fn escape_ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// Columns section of describe (shared by `describe_table` and
/// `describe_table_full` so both return identical column projections).
pub(crate) fn describe_columns_query() -> &'static str {
    "SELECT COLUMN_NAME,DATA_TYPE,IS_NULLABLE,ORDINAL_POSITION \
     FROM INFORMATION_SCHEMA.COLUMNS \
     WHERE TABLE_SCHEMA=@P1 AND TABLE_NAME=@P2 \
     ORDER BY ORDINAL_POSITION"
}

/// Primary-key section of describe (parameterized by schema/table).
pub(crate) fn describe_pk_query() -> &'static str {
    "SELECT kcu.COLUMN_NAME \
     FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
     JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
       ON tc.CONSTRAINT_NAME=kcu.CONSTRAINT_NAME \
      AND tc.TABLE_SCHEMA=kcu.TABLE_SCHEMA \
     WHERE tc.TABLE_SCHEMA=@P1 AND tc.TABLE_NAME=@P2 \
       AND tc.CONSTRAINT_TYPE='PRIMARY KEY' \
     ORDER BY kcu.ORDINAL_POSITION"
}

/// Foreign-key section of describe (parameterized by schema/table).
pub(crate) fn describe_fk_query() -> &'static str {
    "SELECT kcu.COLUMN_NAME,kcu2.TABLE_SCHEMA,kcu2.TABLE_NAME,kcu2.COLUMN_NAME \
     FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS rc \
     JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
       ON rc.CONSTRAINT_NAME=kcu.CONSTRAINT_NAME \
     JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu2 \
       ON rc.UNIQUE_CONSTRAINT_NAME=kcu2.CONSTRAINT_NAME \
      AND kcu.ORDINAL_POSITION=kcu2.ORDINAL_POSITION \
     WHERE kcu.TABLE_SCHEMA=@P1 AND kcu.TABLE_NAME=@P2 \
     ORDER BY kcu.ORDINAL_POSITION"
}

/// View-definition section of describe (parameterized by schema/table).
pub(crate) fn describe_view_query() -> &'static str {
    "SELECT VIEW_DEFINITION FROM INFORMATION_SCHEMA.VIEWS \
     WHERE TABLE_SCHEMA=@P1 AND TABLE_NAME=@P2"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_like_leaves_plain_text_untouched() {
        assert_eq!(escape_like_pattern("email"), "email");
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like_pattern("100%_off"), "100[%][_]off");
    }

    #[test]
    fn escape_like_escapes_brackets() {
        assert_eq!(escape_like_pattern("a[b]c"), "a[[]b[]]c");
    }

    #[test]
    fn escape_ident_brackets_and_doubles_close() {
        assert_eq!(escape_ident("dbo"), "[dbo]");
        assert_eq!(escape_ident("we]ird"), "[we]]ird]");
    }

    #[test]
    fn search_columns_query_filters_by_like_param() {
        let q = search_columns_query();
        assert!(
            q.contains("COLUMN_NAME LIKE @P1"),
            "search must use parameterized LIKE, got: {q}"
        );
        assert!(
            q.contains("INFORMATION_SCHEMA.COLUMNS"),
            "search must read COLUMNS, got: {q}"
        );
    }

    #[test]
    fn list_tables_query_selects_table_type() {
        let q = list_tables_query();
        assert!(
            q.contains("TABLE_TYPE"),
            "list must select TABLE_TYPE, got: {q}"
        );
    }

    #[test]
    fn describe_sections_stay_parameterized_and_complete() {
        // P1a-5: describe_table_full must keep the same six sections over one
        // connection: columns + PK + FK + VIEW + TOP5 sample + bounded COUNT.
        // Every INFORMATION_SCHEMA section stays parameterized (@P1/@P2) so no
        // table name is ever interpolated; only the TOP5/COUNT qualified name
        // is escaped via escape_ident.
        for q in [
            describe_columns_query(),
            describe_pk_query(),
            describe_fk_query(),
            describe_view_query(),
        ] {
            assert!(
                q.contains("@P1") && q.contains("@P2"),
                "describe section must stay parameterized, got: {q}"
            );
        }
        assert!(
            describe_columns_query().contains("INFORMATION_SCHEMA.COLUMNS"),
            "columns section must read COLUMNS"
        );
        assert!(
            describe_pk_query().contains("PRIMARY KEY"),
            "PK section must filter PRIMARY KEY"
        );
        assert!(
            describe_fk_query().contains("REFERENTIAL_CONSTRAINTS"),
            "FK section must read REFERENTIAL_CONSTRAINTS"
        );
        assert!(
            describe_view_query().contains("INFORMATION_SCHEMA.VIEWS"),
            "view section must read VIEWS"
        );
    }

    #[test]
    fn describe_columns_query_matches_single_table_projection() {
        // describe_table and describe_table_full share this projection, so the
        // single-connection refactor cannot drift column sections apart.
        let q = describe_columns_query();
        for col in [
            "COLUMN_NAME",
            "DATA_TYPE",
            "IS_NULLABLE",
            "ORDINAL_POSITION",
        ] {
            assert!(q.contains(col), "columns query must select {col}, got: {q}");
        }
    }
}
