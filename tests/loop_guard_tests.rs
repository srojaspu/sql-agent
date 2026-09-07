//! Loop-guard regression tests (slice 1b): bounded schema hints + same-call dedup.
//! All offline; no live database needed.

use chrono::{Duration, Utc};
use serde_json::json;
use std::collections::HashMap;

use sql_agent::agent::{build_schema_hint_text, dedup_key, MAX_HINTS, MAX_HINT_CHARS};
use sql_agent::database::schema::{SchemaMemory, SchemaMemoryEntry};
use sql_agent::database::TableInfo;

fn table(name: &str) -> TableInfo {
    TableInfo {
        schema: "dbo".into(),
        table: name.into(),
        table_type: "BASE TABLE".into(),
    }
}

fn entry(name: &str, age_secs: i64) -> SchemaMemoryEntry {
    let mut e = SchemaMemoryEntry::new(table(name), vec![], vec![format!("syn-{name}")], 3600);
    e.last_used = Utc::now() - Duration::seconds(age_secs);
    e
}

#[test]
fn hint_overflow_keeps_eight_most_recent_with_marker() {
    assert_eq!(MAX_HINTS, 8);
    let mut memory: SchemaMemory = HashMap::new();
    for i in 0..10 {
        // Tab00 is the most recent, Tab09 the oldest.
        memory.insert(
            format!("dbo.Tab{i:02}"),
            entry(&format!("Tab{i:02}"), i * 60),
        );
    }
    let out = build_schema_hint_text(&memory);
    assert!(
        out.contains("dbo.Tab00"),
        "most recent hint kept, got: {out}"
    );
    assert!(out.contains("dbo.Tab07"), "8th hint kept, got: {out}");
    assert!(!out.contains("dbo.Tab08"), "9th hint trimmed, got: {out}");
    assert!(
        !out.contains("dbo.Tab09"),
        "oldest hint trimmed, got: {out}"
    );
    assert!(
        out.contains("[hints truncated]"),
        "marker appended, got: {out}"
    );
}

#[test]
fn hint_within_cap_has_no_marker() {
    let mut memory: SchemaMemory = HashMap::new();
    memory.insert("dbo.Usuario".into(), entry("Usuario", 10));
    memory.insert("dbo.Producto".into(), entry("Producto", 20));
    let out = build_schema_hint_text(&memory);
    assert!(out.contains("dbo.Usuario"), "got: {out}");
    assert!(out.contains("dbo.Producto"), "got: {out}");
    assert!(
        !out.contains("[hints truncated]"),
        "no trim, no marker, got: {out}"
    );
}

#[test]
fn hint_char_cap_truncates_with_marker() {
    assert_eq!(MAX_HINT_CHARS, 2000);
    let mut memory: SchemaMemory = HashMap::new();
    let mut e = entry("Big", 0);
    e.synonyms = vec!["x".repeat(3000)];
    memory.insert("dbo.Big".into(), e);
    let out = build_schema_hint_text(&memory);
    assert!(
        out.contains("[hints truncated]"),
        "marker appended, got len {}",
        out.len()
    );
    assert!(
        out.len() <= MAX_HINT_CHARS + 64,
        "bounded, got len {}",
        out.len()
    );
}

#[test]
fn dedup_key_stable_for_repeat_call() {
    let args = json!({"table": "dbo.Usuario"});
    assert_eq!(
        dedup_key("describe_table", &args),
        dedup_key("describe_table", &args)
    );
}

#[test]
fn dedup_key_differs_for_different_args_or_tool() {
    let a = json!({"table": "dbo.Usuario"});
    let b = json!({"table": "dbo.Producto"});
    assert_ne!(
        dedup_key("describe_table", &a),
        dedup_key("describe_table", &b)
    );
    assert_ne!(
        dedup_key("describe_table", &a),
        dedup_key("list_tables", &a)
    );
}
