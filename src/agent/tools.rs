use serde_json::json;
use crate::llm::{ToolDefinition, ToolFunctionDefinition};

pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunctionDefinition {
                name: "search_schema".to_string(),
                description: "Busca tablas de SQL Server relacionadas con un término.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Concepto a buscar"
                        }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunctionDefinition {
                name: "describe_table".to_string(),
                description: "Obtiene las columnas y tipos de datos de una tabla.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "table": {
                            "type": "string",
                            "description": "Nombre de la tabla"
                        }
                    },
                    "required": ["table"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunctionDefinition {
                name: "execute_read_query".to_string(),
                description: "Ejecuta una consulta SQL exclusivamente de lectura.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "sql": {
                            "type": "string",
                            "description": "Consulta SELECT a ejecutar"
                        }
                    },
                    "required": ["sql"]
                }),
            },
        },
    ]
}
