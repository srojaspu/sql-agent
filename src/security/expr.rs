//! Expression and function validation plus small word helpers.
//!
//! Pure move from `security::validator` (slice E, step 2): no rule, message,
//! limit, or ordering change. Covers `validate_expr` / `validate_function*`
//! plus [`is_blocked_system_func`], [`is_at_variable`] and
//! [`contains_word`] (the latter stays `pub(crate)` for the facade's
//! denylist pre-check).

use sqlparser::ast::{
    Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentClause, FunctionArguments,
    HavingBound, JsonPathElem, Subscript, WindowType,
};

use crate::error::ValidationBlocked;
use crate::security::tables::ValidationContext;
use crate::security::SqlValidator;

/// Local `bail!` equivalent producing `ValidationBlocked` with identical messages.
macro_rules! blocked {
    ($($arg:tt)*) => {
        return Err(ValidationBlocked::Blocked(format!($($arg)*)))
    };
}

impl SqlValidator {
    /// Recursively walk an expression so subqueries hidden in projection,
    /// WHERE/HAVING, GROUP/ORDER BY, LIMIT/OFFSET, CASE, function arguments,
    /// or JOIN conditions re-enter query validation under budget.
    pub(crate) fn validate_expr(
        &self,
        expr: &Expr,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match expr {
            // Leaves: no nested expressions or queries.
            Expr::Identifier(ident) => {
                if is_at_variable(&ident.value) {
                    blocked!("Variable de sistema bloqueada: {}", ident.value);
                }
                Ok(())
            }
            Expr::CompoundIdentifier(idents) => {
                for ident in idents {
                    if is_at_variable(&ident.value) {
                        blocked!("Variable de sistema bloqueada: {}", ident.value);
                    }
                }
                Ok(())
            }
            Expr::Value(_)
            | Expr::TypedString { .. }
            | Expr::IntroducedString { .. }
            | Expr::Wildcard(_)
            | Expr::QualifiedWildcard(_, _)
            | Expr::MatchAgainst { .. } => Ok(()),
            // Single boxed expressions.
            Expr::IsFalse(e)
            | Expr::IsNotFalse(e)
            | Expr::IsTrue(e)
            | Expr::IsNotTrue(e)
            | Expr::IsNull(e)
            | Expr::IsNotNull(e)
            | Expr::IsUnknown(e)
            | Expr::IsNotUnknown(e)
            | Expr::Nested(e)
            | Expr::OuterJoin(e)
            | Expr::Prior(e)
            | Expr::Cast { expr: e, .. }
            | Expr::Extract { expr: e, .. }
            | Expr::Ceil { expr: e, .. }
            | Expr::Floor { expr: e, .. }
            | Expr::CompositeAccess { expr: e, .. }
            | Expr::Collate { expr: e, .. }
            | Expr::UnaryOp { expr: e, .. }
            | Expr::Named { expr: e, .. } => self.validate_expr(e, ctx),
            Expr::Lambda(l) => self.validate_expr(&l.body, ctx),
            // Pairs of expressions.
            Expr::IsDistinctFrom(a, b)
            | Expr::IsNotDistinctFrom(a, b)
            | Expr::BinaryOp {
                left: a, right: b, ..
            }
            | Expr::AnyOp {
                left: a, right: b, ..
            }
            | Expr::AllOp {
                left: a, right: b, ..
            } => {
                self.validate_expr(a, ctx)?;
                self.validate_expr(b, ctx)
            }
            // Subqueries re-enter query validation under budget.
            Expr::Subquery(query)
            | Expr::Exists {
                subquery: query, ..
            } => self.validate_nested_query(query, ctx),
            Expr::InSubquery { expr, subquery, .. } => {
                self.validate_expr(expr, ctx)?;
                self.validate_nested_query(subquery, ctx)
            }
            Expr::InList { expr, list, .. } => {
                self.validate_expr(expr, ctx)?;
                for item in list {
                    self.validate_expr(item, ctx)?;
                }
                Ok(())
            }
            Expr::InUnnest {
                expr, array_expr, ..
            } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(array_expr, ctx)
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(low, ctx)?;
                self.validate_expr(high, ctx)
            }
            Expr::Like { expr, pattern, .. }
            | Expr::ILike { expr, pattern, .. }
            | Expr::SimilarTo { expr, pattern, .. }
            | Expr::RLike { expr, pattern, .. } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(pattern, ctx)
            }
            Expr::Convert { expr, styles, .. } => {
                self.validate_expr(expr, ctx)?;
                for style in styles {
                    self.validate_expr(style, ctx)?;
                }
                Ok(())
            }
            Expr::AtTimeZone {
                timestamp,
                time_zone,
            } => {
                self.validate_expr(timestamp, ctx)?;
                self.validate_expr(time_zone, ctx)
            }
            Expr::Position { expr, r#in } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(r#in, ctx)
            }
            Expr::Substring {
                expr,
                substring_from,
                substring_for,
                ..
            } => {
                self.validate_expr(expr, ctx)?;
                if let Some(from) = substring_from {
                    self.validate_expr(from, ctx)?;
                }
                if let Some(for_) = substring_for {
                    self.validate_expr(for_, ctx)?;
                }
                Ok(())
            }
            Expr::Trim {
                expr,
                trim_what,
                trim_characters,
                ..
            } => {
                self.validate_expr(expr, ctx)?;
                if let Some(what) = trim_what {
                    self.validate_expr(what, ctx)?;
                }
                if let Some(chars) = trim_characters {
                    for c in chars {
                        self.validate_expr(c, ctx)?;
                    }
                }
                Ok(())
            }
            Expr::Overlay {
                expr,
                overlay_what,
                overlay_from,
                overlay_for,
            } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(overlay_what, ctx)?;
                self.validate_expr(overlay_from, ctx)?;
                if let Some(for_) = overlay_for {
                    self.validate_expr(for_, ctx)?;
                }
                Ok(())
            }
            Expr::JsonAccess { value, path } => {
                self.validate_expr(value, ctx)?;
                for elem in &path.path {
                    if let JsonPathElem::Bracket { key } = elem {
                        self.validate_expr(key, ctx)?;
                    }
                }
                Ok(())
            }
            Expr::MapAccess { column, keys } => {
                self.validate_expr(column, ctx)?;
                for k in keys {
                    self.validate_expr(&k.key, ctx)?;
                }
                Ok(())
            }
            Expr::Subscript { expr, subscript } => {
                self.validate_expr(expr, ctx)?;
                match subscript.as_ref() {
                    Subscript::Index { index } => self.validate_expr(index, ctx),
                    Subscript::Slice {
                        lower_bound,
                        upper_bound,
                        stride,
                    } => {
                        if let Some(e) = lower_bound {
                            self.validate_expr(e, ctx)?;
                        }
                        if let Some(e) = upper_bound {
                            self.validate_expr(e, ctx)?;
                        }
                        if let Some(e) = stride {
                            self.validate_expr(e, ctx)?;
                        }
                        Ok(())
                    }
                }
            }
            Expr::Case {
                operand,
                conditions,
                results,
                else_result,
            } => {
                if let Some(op) = operand {
                    self.validate_expr(op, ctx)?;
                }
                for c in conditions {
                    self.validate_expr(c, ctx)?;
                }
                for r in results {
                    self.validate_expr(r, ctx)?;
                }
                if let Some(e) = else_result {
                    self.validate_expr(e, ctx)?;
                }
                Ok(())
            }
            Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) => {
                for set in sets {
                    for e in set {
                        self.validate_expr(e, ctx)?;
                    }
                }
                Ok(())
            }
            Expr::Tuple(exprs) | Expr::Struct { values: exprs, .. } => {
                for e in exprs {
                    self.validate_expr(e, ctx)?;
                }
                Ok(())
            }
            Expr::Dictionary(fields) => {
                for f in fields {
                    self.validate_expr(&f.value, ctx)?;
                }
                Ok(())
            }
            Expr::Map(m) => {
                for entry in &m.entries {
                    self.validate_expr(&entry.key, ctx)?;
                    self.validate_expr(&entry.value, ctx)?;
                }
                Ok(())
            }
            Expr::Array(a) => {
                for e in &a.elem {
                    self.validate_expr(e, ctx)?;
                }
                Ok(())
            }
            Expr::Interval(i) => self.validate_expr(&i.value, ctx),
            Expr::Function(f) => self.validate_function(f, ctx),
            Expr::Method(m) => {
                self.validate_expr(&m.expr, ctx)?;
                for f in &m.method_chain {
                    self.validate_function(f, ctx)?;
                }
                Ok(())
            }
        }
    }

    pub(crate) fn validate_function(
        &self,
        func: &Function,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        let name = func.name.0.last().map(|i| i.value.as_str()).unwrap_or("");
        if is_blocked_system_func(name) {
            blocked!("Función de sistema bloqueada: {name}");
        }
        self.validate_function_args(&func.parameters, ctx)?;
        self.validate_function_args(&func.args, ctx)?;
        if let Some(filter) = &func.filter {
            self.validate_expr(filter, ctx)?;
        }
        if let Some(WindowType::WindowSpec(spec)) = &func.over {
            for e in &spec.partition_by {
                self.validate_expr(e, ctx)?;
            }
            for obe in &spec.order_by {
                self.validate_expr(&obe.expr, ctx)?;
            }
        }
        for obe in &func.within_group {
            self.validate_expr(&obe.expr, ctx)?;
        }
        Ok(())
    }

    pub(crate) fn validate_function_args(
        &self,
        args: &FunctionArguments,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match args {
            FunctionArguments::None => Ok(()),
            FunctionArguments::Subquery(query) => self.validate_nested_query(query, ctx),
            FunctionArguments::List(list) => {
                for arg in &list.args {
                    match arg {
                        FunctionArg::Named { arg, .. } | FunctionArg::ExprNamed { arg, .. } => {
                            self.validate_function_arg_expr(arg, ctx)?
                        }
                        FunctionArg::Unnamed(arg) => self.validate_function_arg_expr(arg, ctx)?,
                    }
                }
                for clause in &list.clauses {
                    match clause {
                        FunctionArgumentClause::OrderBy(exprs) => {
                            for obe in exprs {
                                self.validate_expr(&obe.expr, ctx)?;
                            }
                        }
                        FunctionArgumentClause::Limit(e) => self.validate_expr(e, ctx)?,
                        FunctionArgumentClause::Having(HavingBound(_, e)) => {
                            self.validate_expr(e, ctx)?
                        }
                        _ => {}
                    }
                }
                Ok(())
            }
        }
    }

    pub(crate) fn validate_function_arg_expr(
        &self,
        arg: &FunctionArgExpr,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match arg {
            FunctionArgExpr::Expr(e) => self.validate_expr(e, ctx),
            FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_) => Ok(()),
        }
    }
}

/// Scalar system-information functions that must never appear, even when the
/// query targets an allowlisted table (P0-2). Identity, host, database, and
/// clock disclosure has no legitimate reporting use through this agent.
/// `@@` variables are denied separately via [`is_at_variable`].
fn is_blocked_system_func(name: &str) -> bool {
    let upper = name
        .trim()
        .trim_matches(|c| c == '[' || c == ']' || c == '"')
        .to_ascii_uppercase();
    let short = upper.rsplit('.').next().unwrap_or(&upper);
    if short.starts_with("@@") {
        return true;
    }
    matches!(
        short,
        "SUSER_SNAME"
            | "SUSER_SID"
            | "SUSER_NAME"
            | "SYSTEM_USER"
            | "SESSION_USER"
            | "ORIGINAL_LOGIN"
            | "HOST_NAME"
            | "HOST_ID"
            | "APP_NAME"
            | "DB_NAME"
            | "DB_ID"
            | "GETDATE"
            | "GETUTCDATE"
            | "SYSDATETIME"
            | "SYSUTCDATETIME"
            | "SYSDATETIMEOFFSET"
    )
}

/// T-SQL system variables (`@@VERSION`, `@@SERVERNAME`, ...) parse as
/// identifiers rather than function calls, so they need their own check.
fn is_at_variable(name: &str) -> bool {
    name.trim_start().starts_with("@@")
}

pub(crate) fn contains_word(sql: &str, word: &str) -> bool {
    sql.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|p| p.eq_ignore_ascii_case(word))
}
