use crate::llm::{FunctionDefinition, ToolDefinition};
use serde_json::json;

fn object(fields: &[(&str, &str, bool)]) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    let mut req = Vec::new();
    for (n, t, r) in fields {
        props.insert((*n).into(), json!({"type": t}));
        if *r {
            req.push((*n).to_string());
        }
    }
    json!({"type":"object","properties":props,"required":req,"additionalProperties":false})
}

pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            r#type: "function",
            function: FunctionDefinition {
                name: "search_schema",
                description: "Busca tablas visibles. Usa terminos normalizados; devuelve top-K rankeado Levenshtein con Did-you-mean si 0.",
                parameters: object(&[("query", "string", true)]),
            },
        },
        ToolDefinition {
            r#type: "function",
            function: FunctionDefinition {
                name: "describe_table",
                description: "Obtiene columnas de una tabla. Usa SOLO nombre calificado exacto (schema.tabla) devuelto por search_schema; no inventes dbo.*.",
                parameters: object(&[("table", "string", true)]),
            },
        },
        ToolDefinition {
            r#type: "function",
            function: FunctionDefinition {
                name: "execute_read_query",
                description: "Ejecuta SELECT validado. Usa solo tablas/campos verificados por search_schema/describe_table; no inventes nombres.",
                parameters: object(&[("sql", "string", true)]),
            },
        },
        ToolDefinition {
            r#type: "function",
            function: FunctionDefinition {
                name: "list_tables",
                description: "Lista todas las tablas y vistas visibles (INFORMATION_SCHEMA). Úsalo para descubrir el esquema completo sin adivinar nombres.",
                parameters: object(&[]),
            },
        },
        ToolDefinition {
            r#type: "function",
            function: FunctionDefinition {
                name: "search_columns",
                description: "Busca columnas por nombre en todo el esquema. Devuelve pares tabla.columna coincidentes; verifica luego con describe_table.",
                parameters: object(&[("query", "string", true)]),
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_has_five_tools() {
        let defs = definitions();
        assert_eq!(defs.len(), 5);
    }

    #[test]
    fn definitions_names_match_ollama() {
        let defs = definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name).collect();
        assert_eq!(
            names,
            vec![
                "search_schema",
                "describe_table",
                "execute_read_query",
                "list_tables",
                "search_columns"
            ]
        );
    }

    #[test]
    fn definitions_grounding_descriptions() {
        let defs = definitions();
        let search_desc = defs[0].function.description;
        assert!(
            search_desc.contains("Did-you-mean")
                || search_desc.contains("Did you mean")
                || search_desc.contains("top-K"),
            "search_schema description must mention Did-you-mean/top-K grounding"
        );
        let describe_desc = defs[1].function.description;
        assert!(
            describe_desc.contains("exacto")
                || describe_desc.contains("EXACTO")
                || describe_desc.contains("no inventes"),
            "describe_table must warn against inventing dbo.*"
        );
        let list_desc = defs[3].function.description;
        assert!(
            list_desc.contains("INFORMATION_SCHEMA") || list_desc.contains("todas"),
            "list_tables must promise full schema listing, got: {list_desc}"
        );
        let search_cols_desc = defs[4].function.description;
        assert!(
            search_cols_desc.contains("columna") || search_cols_desc.contains("column"),
            "search_columns must describe column search, got: {search_cols_desc}"
        );
    }

    #[test]
    fn search_columns_requires_query_param() {
        let defs = definitions();
        let params = &defs[4].function.parameters;
        let required = params
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            required.iter().any(|v| v == "query"),
            "search_columns must require 'query', got: {params}"
        );
        let props = params
            .get("properties")
            .and_then(|p| p.get("query"))
            .cloned()
            .unwrap_or_default();
        assert_eq!(props.get("type").and_then(|t| t.as_str()), Some("string"));
    }
}
