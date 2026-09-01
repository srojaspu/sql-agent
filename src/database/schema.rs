use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct TableInfo {
    pub schema: String,
    pub table: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub column: String,
    pub data_type: String,
    pub nullable: bool,
    pub ordinal: i32,
}
