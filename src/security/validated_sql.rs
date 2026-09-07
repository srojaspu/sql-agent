//! Validated SQL newtype: the single security seam.
//!
//! A [`ValidatedSql`] proves the contained text passed
//! [`SqlValidator::validate`]. The inner [`String`] is private so no caller
//! can forge it: the only public construction path is
//! [`SqlValidator::validate`] (plus [`ValidatedSql::parse`], which forwards
//! to it). The database layer accepts only this type by value, never `&str`
//! or [`String`], so every `execute_read` call site is preceded by validation
//! by construction.
//!
//! Accessors are intentionally narrow:
//!
//! * [`ValidatedSql::as_str`] borrows the approved text for audit/logging.
//! * [`ValidatedSql::into_string`] moves it out for the single DB call.
//!
//! The approved text is the trimmed input the validator checked, so audit
//! and execution observe byte-identical SQL.

use crate::error::ValidationBlocked;
use crate::security::SqlValidator;

/// Approved SQL text: constructed only via [`SqlValidator::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSql(String);

impl ValidatedSql {
    /// Crate-internal forge point, called only by `SqlValidator::validate`.
    ///
    /// Not public: external code (and most of the crate) cannot name it, so
    /// the validator stays the sole public construction path.
    pub(crate) fn from_trusted(sql: String) -> Self {
        Self(sql)
    }

    /// Borrow the approved SQL text (audit, logging, DB driver).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Move the approved SQL text out (single DB execution).
    pub fn into_string(self) -> String {
        self.0
    }

    /// Validate `sql` and wrap the approved text.
    ///
    /// Thin forwarder over [`SqlValidator::validate`] so dispatcher code reads
    /// as `ValidatedSql::parse(&validator, sql)?`; the real checks live in
    /// the validator and the inner string is built there.
    pub fn parse(validator: &SqlValidator, sql: &str) -> Result<Self, ValidationBlocked> {
        validator.validate(sql)
    }
}

impl std::fmt::Display for ValidatedSql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;

    fn policy() -> SecurityPolicy {
        SecurityPolicy {
            max_sql_length: 10_000,
            allowed_tables: vec!["dbo.entradaLote".into()],
            block_sensitive_columns: true,
            block_comments: true,
            allow_cte: true,
            allow_system_tables: false,
            max_joins: 5,
            max_subqueries: 5,
        }
    }

    #[test]
    fn parse_returns_trimmed_approved_text() {
        let v = SqlValidator::new(policy());
        let validated = ValidatedSql::parse(&v, "  SELECT TOP 10 * FROM dbo.entradaLote  ")
            .expect("allowed SQL must validate");
        assert_eq!(validated.as_str(), "SELECT TOP 10 * FROM dbo.entradaLote");
    }
}
