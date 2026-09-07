//! PII redaction for audit payloads and persisted sessions.
//!
//! Pure leaf module: no `agent::*` imports. Both the audit sink and the
//! session store share these helpers so the redaction word list has a
//! single source of truth.

/// Redact a free-text string when it mentions a sensitive keyword.
///
/// The keyword list is security policy (do not extend here without a
/// security review); matching stays case-insensitive substring-based.
pub fn redact_content(content: &str) -> String {
    let lower = content.to_ascii_lowercase();
    let sensitive = [
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "private_key",
        "client_secret",
        "access_token",
        "refresh_token",
    ];
    for word in sensitive {
        if lower.contains(word) {
            return "[REDACTED sensitive content]".to_string();
        }
    }
    content.to_string()
}

/// Recursively redact sensitive strings in a JSON payload so SQL text and
/// user questions never persist PII/secrets verbatim.
pub fn redact_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(redact_content(&s)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(redact_value).collect())
        }
        serde_json::Value::Object(map) => {
            serde_json::Value::Object(map.into_iter().map(|(k, v)| (k, redact_value(v))).collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_sensitive_replaces_password() {
        let c = "SELECT password FROM users";
        assert_eq!(redact_content(c), "[REDACTED sensitive content]");
        let ok = "SELECT name FROM users";
        assert_eq!(redact_content(ok), ok);
    }

    #[test]
    fn redact_value_recurses_into_objects_and_arrays() {
        let v = redact_value(json!({
            "question": "what is the token?",
            "nested": { "sql": "SELECT secret FROM t" },
            "list": ["SELECT name FROM t", "leak api_key here"],
            "n": 42
        }));
        assert_eq!(
            v["question"],
            json!("[REDACTED sensitive content]"),
            "top-level string redacted"
        );
        assert_eq!(
            v["nested"]["sql"],
            json!("[REDACTED sensitive content]"),
            "nested string redacted"
        );
        assert_eq!(
            v["list"][0],
            json!("SELECT name FROM t"),
            "safe string preserved"
        );
        assert_eq!(
            v["list"][1],
            json!("[REDACTED sensitive content]"),
            "array item redacted"
        );
        assert_eq!(v["n"], json!(42), "non-strings untouched");
    }
}
