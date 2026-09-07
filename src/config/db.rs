//! SQL Server connection settings.
//!
//! Sourced from `DATABASE_*` environment variables by [`crate::config::loader`].
//! Every default and validation message below matches the pre-split
//! `config.rs` behavior byte-for-byte; this file only gives the fields a
//! home of their own.

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
