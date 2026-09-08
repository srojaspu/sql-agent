//! LLM-facing text formatters plus small text helpers (pure domain logic).
//!
//! Extracted verbatim from `agent::core` (slice D, step 1): table/column/result
//! formatters, the invalid-object re-inject builder, `limit_text`,
//! `looks_like_sql` and `split_table` (+ `split_table_pub`).
//!
//! This module re-exports from sibling modules for backward compatibility.

#[allow(unused_imports)]
pub use super::export::{csv_escape, export_messages_csv, export_messages_json};
#[allow(unused_imports)]
pub use super::formatters::{
    extract_sql, format_cell_value, format_column_matches, format_distinct_values,
    format_invalid_reinject, format_query_result, format_table_detail, format_table_list,
    limit_text, looks_like_sql, split_table, split_table_pub, tool_error_result,
};
pub use super::history::format_history_readable;
