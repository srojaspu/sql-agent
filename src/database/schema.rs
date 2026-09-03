use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub table: String,
    /// TABLE_TYPE from INFORMATION_SCHEMA.TABLES: "BASE TABLE" or "VIEW".
    pub table_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMatch {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub data_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKeyInfo {
    pub column: String,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_column: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDetail {
    pub columns: Vec<ColumnInfo>,
    pub primary_keys: Vec<String>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    pub view_definition: Option<String>,
    pub sample_rows: Vec<serde_json::Value>,
    pub row_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub column: String,
    pub data_type: String,
    pub nullable: bool,
    pub ordinal: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaMemoryEntry {
    pub table: TableInfo,
    pub columns: Vec<ColumnInfo>,
    pub synonyms: Vec<String>,
    pub last_used: DateTime<Utc>,
    pub hit_count: u64,
    pub ttl_seconds: u64,
}

impl SchemaMemoryEntry {
    pub fn new(
        table: TableInfo,
        columns: Vec<ColumnInfo>,
        synonyms: Vec<String>,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            table,
            columns,
            synonyms,
            last_used: Utc::now(),
            hit_count: 1,
            ttl_seconds,
        }
    }

    pub fn is_expired(&self) -> bool {
        let elapsed = Utc::now()
            .signed_duration_since(self.last_used)
            .num_seconds() as u64;
        elapsed > self.ttl_seconds
    }

    pub fn touch(&mut self) {
        self.last_used = Utc::now();
        self.hit_count += 1;
    }

    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }
}

pub type SchemaMemory = HashMap<String, SchemaMemoryEntry>;

/// Return true if entry is expired given now (pure helper for tests)
pub fn is_expired_at(entry: &SchemaMemoryEntry, now: DateTime<Utc>) -> bool {
    let elapsed = now.signed_duration_since(entry.last_used).num_seconds() as u64;
    elapsed > entry.ttl_seconds
}

/// Invalidate all entries in memory (for /refresh)
pub fn invalidate_all(memory: &mut SchemaMemory) {
    memory.clear();
}

/// Insert or update schema memory with synonym tracking
pub fn upsert_schema_memory(
    memory: &mut SchemaMemory,
    key: String,
    table: TableInfo,
    columns: Vec<ColumnInfo>,
    synonym: Option<String>,
    ttl_seconds: u64,
) {
    if let Some(existing) = memory.get_mut(&key) {
        existing.last_used = Utc::now();
        existing.hit_count += 1;
        if let Some(s) = synonym {
            if !existing.synonyms.contains(&s) {
                existing.synonyms.push(s);
            }
        }
        // refresh columns if provided
        if !columns.is_empty() {
            existing.columns = columns;
        }
        existing.ttl_seconds = ttl_seconds;
    } else {
        let mut entry = SchemaMemoryEntry::new(table, columns, Vec::new(), ttl_seconds);
        if let Some(s) = synonym {
            entry.synonyms.push(s);
        }
        memory.insert(key, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_table() -> TableInfo {
        TableInfo {
            schema: "dbo".into(),
            table: "Usuario".into(),
            table_type: "BASE TABLE".into(),
        }
    }

    #[test]
    fn table_info_carries_table_type() {
        let t = make_table();
        assert_eq!(t.table_type, "BASE TABLE");
    }

    #[test]
    fn table_info_view_type_survives_serde_roundtrip() {
        let t = TableInfo {
            schema: "dbo".into(),
            table: "VwActive".into(),
            table_type: "VIEW".into(),
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["table_type"], "VIEW");
        let back: TableInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back.table_type, "VIEW");
        assert_eq!(back.table, "VwActive");
    }

    #[test]
    fn schema_memory_entry_new_has_hit_1() {
        let e = SchemaMemoryEntry::new(make_table(), vec![], vec!["usuarios".into()], 300);
        assert_eq!(e.hit_count, 1);
        assert_eq!(e.synonyms, vec!["usuarios"]);
        assert!(!e.is_expired());
    }

    #[test]
    fn schema_memory_entry_ttl_expired() {
        let mut e = SchemaMemoryEntry::new(make_table(), vec![], vec![], 1);
        // Simulate expired by moving last_used back 5 seconds
        e.last_used = Utc::now() - Duration::seconds(5);
        assert!(e.is_expired(), "should be expired after 5s with ttl 1");
        assert!(!e.is_valid());
    }

    #[test]
    fn schema_memory_entry_touch_resets_ttl() {
        let mut e = SchemaMemoryEntry::new(make_table(), vec![], vec![], 300);
        e.last_used = Utc::now() - Duration::seconds(400);
        assert!(e.is_expired());
        e.touch();
        assert!(!e.is_expired());
        assert_eq!(e.hit_count, 2);
    }

    #[test]
    fn upsert_tracks_synonyms_and_hit_count() {
        let mut mem: SchemaMemory = HashMap::new();
        let tbl = make_table();
        upsert_schema_memory(
            &mut mem,
            "dbo.Usuario".into(),
            tbl.clone(),
            vec![],
            Some("usuarios".into()),
            300,
        );
        assert_eq!(mem["dbo.Usuario"].hit_count, 1);
        assert_eq!(mem["dbo.Usuario"].synonyms, vec!["usuarios"]);
        // Second upsert with different synonym
        upsert_schema_memory(
            &mut mem,
            "dbo.Usuario".into(),
            tbl.clone(),
            vec![],
            Some("usuario".into()),
            300,
        );
        assert_eq!(mem["dbo.Usuario"].hit_count, 2);
        assert!(mem["dbo.Usuario"].synonyms.contains(&"usuarios".into()));
        assert!(mem["dbo.Usuario"].synonyms.contains(&"usuario".into()));
    }

    #[test]
    fn invalidate_all_clears() {
        let mut mem: SchemaMemory = HashMap::new();
        let tbl = make_table();
        upsert_schema_memory(&mut mem, "dbo.Usuario".into(), tbl, vec![], None, 300);
        assert_eq!(mem.len(), 1);
        invalidate_all(&mut mem);
        assert!(mem.is_empty());
    }

    #[test]
    fn is_expired_at_pure_helper() {
        let mut e = SchemaMemoryEntry::new(make_table(), vec![], vec![], 300);
        e.last_used = Utc::now() - Duration::seconds(10);
        let now = Utc::now();
        assert!(
            !is_expired_at(&e, now),
            "10s elapsed with ttl 300 should not be expired"
        );
        e.last_used = now - Duration::seconds(400);
        assert!(is_expired_at(&e, now));
    }
}
