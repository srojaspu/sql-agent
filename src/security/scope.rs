//! Alias-aware wildcard scope plus its allowlist gate.
//!
//! Pure move from `security::validator` (slice E, step 2): no rule, message,
//! limit, or ordering change. [`Scope`] resolves `*` / `alias.*` to known
//! FROM/JOIN tables; [`SqlValidator::check_wildcard_scope`] reuses the
//! prebuilt allowlist from [`ValidationContext`](super::tables::ValidationContext).

use std::collections::HashMap;

use sqlparser::ast::{ObjectName, SelectItem, TableFactor, TableWithJoins};

use crate::error::ValidationBlocked;
use crate::security::tables::ValidationContext;
use crate::security::SqlValidator;
use crate::util::normalize_table_name;

/// Local `bail!` equivalent producing `ValidationBlocked` with identical messages.
macro_rules! blocked {
    ($($arg:tt)*) => {
        return Err(ValidationBlocked::Blocked(format!($($arg)*)))
    };
}

/// Alias-aware resolution of the tables visible to one SELECT scope.
/// Built from FROM plus JOIN relations so `*` and `alias.*` can be
/// scoped to known tables before the allowlist check runs.
#[derive(Default)]
pub(crate) struct Scope {
    /// Normalized base table names visible in this scope.
    pub(crate) tables: Vec<String>,
    /// Normalized qualifier (alias, full name, or short name) to the base
    /// table it refers to. `None` marks a derived table or CTE alias, which
    /// has no base table of its own.
    qualifiers: HashMap<String, Option<String>>,
}

impl Scope {
    pub(crate) fn collect(from: &[TableWithJoins]) -> Self {
        let mut scope = Scope::default();
        for twj in from {
            scope.collect_factor(&twj.relation);
            for join in &twj.joins {
                scope.collect_factor(&join.relation);
            }
        }
        scope
    }

    fn collect_factor(&mut self, factor: &TableFactor) {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let base = name.to_string();
                let norm = normalize_table_name(&base);
                if !self.tables.iter().any(|t| normalize_table_name(t) == norm) {
                    self.tables.push(base.clone());
                }
                self.qualifiers
                    .entry(norm)
                    .or_insert_with(|| Some(base.clone()));
                if let Some(short) = name.0.last() {
                    self.qualifiers
                        .entry(normalize_table_name(&short.value))
                        .or_insert_with(|| Some(base.clone()));
                }
                if let Some(a) = alias {
                    self.qualifiers
                        .entry(normalize_table_name(&a.name.value))
                        .or_insert_with(|| Some(base));
                }
            }
            TableFactor::Derived { alias: Some(a), .. } => {
                self.qualifiers
                    .entry(normalize_table_name(&a.name.value))
                    .or_insert(None);
            }
            TableFactor::Derived { .. } => {}
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.collect_factor(&table_with_joins.relation);
                for join in &table_with_joins.joins {
                    self.collect_factor(&join.relation);
                }
            }
            _ => {}
        }
    }

    /// Resolve a qualified wildcard prefix (`alias`, `table`, or
    /// `schema.table`) to its base table, if the qualifier is known.
    /// Returns `None` when the qualifier matches nothing in scope.
    fn resolve(&self, name: &ObjectName) -> Option<Option<String>> {
        if let Some(r) = self
            .qualifiers
            .get(&normalize_table_name(&name.to_string()))
        {
            return Some(r.clone());
        }
        if let Some(last) = name.0.last() {
            if let Some(r) = self.qualifiers.get(&normalize_table_name(&last.value)) {
                return Some(r.clone());
            }
        }
        if let Some(first) = name.0.first() {
            if let Some(r) = self.qualifiers.get(&normalize_table_name(&first.value)) {
                return Some(r.clone());
            }
        }
        None
    }
}

impl SqlValidator {
    /// Every wildcard in the projection must resolve to a known scope, and
    /// every scoped base table must be allowlisted (CTEs excluded, mirroring
    /// the global allowlist check).
    pub(crate) fn check_wildcard_scope(
        &self,
        projection: &[SelectItem],
        scope: &Scope,
        ctx: &ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        let mut has_wildcard = false;
        for item in projection {
            match item {
                SelectItem::Wildcard(_) => has_wildcard = true,
                SelectItem::QualifiedWildcard(name, _) => {
                    has_wildcard = true;
                    if scope.resolve(name).is_none() {
                        blocked!("Wildcard con alcance desconocido: {name}");
                    }
                }
                SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. } => {}
            }
        }
        if has_wildcard {
            // Reuse the prebuilt allowlist from the validation context instead
            // of rebuilding the HashSet per SELECT scope.
            if let Some(allowed) = &ctx.allowed {
                for table in &scope.tables {
                    if ctx.ctes.contains(&normalize_table_name(table)) {
                        continue;
                    }
                    if !allowed.contains(&normalize_table_name(table)) {
                        blocked!("Tabla no permitida: {table}");
                    }
                }
            }
        }
        Ok(())
    }
}
