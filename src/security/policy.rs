use crate::config::Config;

#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub max_rows: usize,
    pub blocked_tables: Vec<String>,
    pub blocked_columns: Vec<String>,
}

impl SecurityPolicy {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_rows: config.max_rows,
            blocked_tables: config.blocked_tables.clone(),
            blocked_columns: config.blocked_columns.clone(),
        }
    }

    pub fn is_table_blocked(&self, table: &str) -> bool {
        let table = table.to_lowercase();
        self.blocked_tables.iter().any(|blocked| table == *blocked)
    }

    pub fn is_column_blocked(&self, column: &str) -> bool {
        let column = column.to_lowercase();
        self.blocked_columns.iter().any(|blocked| column == *blocked)
    }
}
