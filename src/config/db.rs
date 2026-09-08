//! SQL Server connection settings.
//!
//! Sourced from `DATABASE_*` environment variables by [`crate::config::loader`].
//! Every default and validation message below matches the pre-split
//! `config.rs` behavior byte-for-byte; this file only gives the fields a
//! home of their own.

use crate::error::ConfigError;

/// SQL Server connection settings (`DATABASE_*`).
#[derive(Clone, Debug)]
pub struct DbConfig {
    /// SQL Server host.
    ///
    /// Env: `DATABASE_HOST` (required, no default).
    pub host: String,

    /// SQL Server TCP port.
    ///
    /// Env: `DATABASE_PORT` (default `1433`).
    pub port: u16,

    /// Initial catalog / database name.
    ///
    /// Env: `DATABASE_NAME` (required, no default).
    pub name: String,

    /// SQL login user.
    ///
    /// Env: `DATABASE_USER` (required, no default).
    pub user: String,

    /// SQL login password.
    ///
    /// Env: `DATABASE_PASSWORD` (required, no default).
    pub password: String,

    /// Accept the server TLS certificate without chain validation.
    ///
    /// Env: `DATABASE_TRUST_CERT` (default `false`).
    ///
    /// Security note: defaults to `false` so production fails closed;
    /// development opts in explicitly with `true`. The read-only permission
    /// gate (`database::sqlserver`) runs regardless of this flag.
    pub trust_cert: bool,
}

/// Validates that a database name contains only safe characters.
///
/// Trims surrounding whitespace, then rejects empty, illegal-character,
/// and overlong (>128) names with a typed [`ConfigError::Invalid`] before
/// any connection attempt. Prevents prompt injection if `DATABASE_NAME`
/// contains newlines or special sequences.
pub fn sanitize_db_name(n: &str) -> Result<String, ConfigError> {
    let name = n.trim();
    if name.is_empty() {
        return Err(ConfigError::Invalid(
            "DATABASE_NAME".into(),
            "no puede estar vacío".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(ConfigError::Invalid(
            "DATABASE_NAME".into(),
            format!(
                "contiene caracteres no permitidos: '{name}'. \
                 Solo se permiten letras, dígitos, '_', '-' y '.'"
            ),
        ));
    }
    if name.len() > 128 {
        return Err(ConfigError::Invalid(
            "DATABASE_NAME".into(),
            "supera el máximo de 128 caracteres".into(),
        ));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_db_name_rejection_table() {
        // Empty names are rejected before any connection attempt.
        assert!(sanitize_db_name("").is_err());
        assert!(sanitize_db_name("   ").is_err());
        // Illegal characters (spaces, punctuation, newlines) are rejected.
        assert!(sanitize_db_name("my db!").is_err());
        assert!(sanitize_db_name("db;DROP").is_err());
        assert!(sanitize_db_name("a\nb").is_err());
        // Overlong names (>128 chars) are rejected.
        let overlong = "a".repeat(129);
        assert!(sanitize_db_name(&overlong).is_err());
    }

    #[test]
    fn sanitize_db_name_accepts_valid_and_trims() {
        assert_eq!(sanitize_db_name("TestDB").unwrap(), "TestDB");
        assert_eq!(sanitize_db_name("  TestDB  ").unwrap(), "TestDB");
        assert_eq!(sanitize_db_name("my-db.v2_01").unwrap(), "my-db.v2_01");
    }
}
