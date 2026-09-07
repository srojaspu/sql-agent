//! Query/table validation plus the shared validation context.
//!
//! Pure move from `security::validator` (slice E, step 2): no rule, message,
//! limit, or ordering change. Covers `validate_query` / nested / set /
//! select / joins / table factors plus [`ValidationContext`]; expression
//! details live in [`super::expr`], wildcard scope in [`super::scope`].

use std::collections::HashSet;

use sqlparser::ast::{
    Distinct, GroupByExpr, JoinConstraint, JoinOperator, Query, Select, SelectItem, SetExpr,
    TableFactor, TableWithJoins, TopQuantity,
};

use crate::error::ValidationBlocked;
use crate::security::scope::Scope;
use crate::security::SqlValidator;
use crate::util::normalize_table_name;

/// Local `bail!` equivalent producing `ValidationBlocked` with identical messages.
macro_rules! blocked {
    ($($arg:tt)*) => {
        return Err(ValidationBlocked::Blocked(format!($($arg)*)))
    };
}

#[derive(Default)]
pub(crate) struct ValidationContext {
    pub(crate) tables: HashSet<String>,
    pub(crate) ctes: HashSet<String>,
    pub(crate) joins: usize,
    pub(crate) subqueries: usize,
    /// Allowlist built once in `validate` and shared with every scope check.
    /// `None` means no allowlist is configured (all tables pass the gate).
    pub(crate) allowed: Option<HashSet<String>>,
}

impl SqlValidator {
    pub(crate) fn validate_query(
        &self,
        query: &Query,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        if let Some(with) = &query.with {
            if !self.policy.allow_cte {
                blocked!("CTE/WITH no permitido");
            }
            for cte in &with.cte_tables {
                ctx.ctes.insert(normalize_table_name(&cte.alias.name.value));
                self.validate_nested_query(&cte.query, ctx)?;
            }
        }
        self.validate_set_expr(&query.body, ctx)?;
        if let Some(order_by) = &query.order_by {
            for obe in &order_by.exprs {
                self.validate_expr(&obe.expr, ctx)?;
            }
        }
        if let Some(limit) = &query.limit {
            self.validate_expr(limit, ctx)?;
        }
        for expr in &query.limit_by {
            self.validate_expr(expr, ctx)?;
        }
        if let Some(offset) = &query.offset {
            self.validate_expr(&offset.value, ctx)?;
        }
        if let Some(fetch) = &query.fetch {
            if let Some(quantity) = &fetch.quantity {
                self.validate_expr(quantity, ctx)?;
            }
        }
        Ok(())
    }

    /// Re-enter validation for a nested query, enforcing the subquery budget.
    pub(crate) fn validate_nested_query(
        &self,
        query: &Query,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        ctx.subqueries += 1;
        if ctx.subqueries > self.policy.max_subqueries {
            blocked!(
                "Demasiadas subconsultas: máximo {}",
                self.policy.max_subqueries
            );
        }
        self.validate_query(query, ctx)
    }

    pub(crate) fn validate_set_expr(
        &self,
        expr: &SetExpr,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match expr {
            SetExpr::Select(select) => self.validate_select(select, ctx),
            SetExpr::SetOperation { left, right, .. } => {
                // Every UNION/EXCEPT/INTERSECT branch must be read-only.
                self.validate_set_expr(left, ctx)?;
                self.validate_set_expr(right, ctx)
            }
            SetExpr::Query(query) => self.validate_nested_query(query, ctx),
            SetExpr::Values(_) | SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Table(_) => {
                blocked!("Expresión SQL no permitida")
            }
        }
    }

    pub(crate) fn validate_select(
        &self,
        select: &Select,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        // SELECT INTO creates a table: it is a write, never read-only.
        if select.into.is_some() {
            blocked!("SELECT INTO no permitido");
        }
        // Without FROM there is no allowlist scope to resolve.
        if select.from.is_empty() {
            blocked!("SELECT sin FROM no permitido");
        }
        // Alias-aware scope for wildcard resolution (FROM + JOINs).
        let scope = Scope::collect(&select.from);
        self.check_wildcard_scope(&select.projection, &scope, ctx)?;
        for item in &select.projection {
            match item {
                SelectItem::UnnamedExpr(expr) => self.validate_expr(expr, ctx)?,
                SelectItem::ExprWithAlias { expr, .. } => self.validate_expr(expr, ctx)?,
                SelectItem::QualifiedWildcard(_, _) | SelectItem::Wildcard(_) => {}
            }
        }
        if let Some(top) = &select.top {
            if let Some(TopQuantity::Expr(expr)) = &top.quantity {
                self.validate_expr(expr, ctx)?;
            }
        }
        if let Some(Distinct::On(exprs)) = &select.distinct {
            for expr in exprs {
                self.validate_expr(expr, ctx)?;
            }
        }
        for twj in &select.from {
            self.validate_table_with_joins(twj, ctx)?;
        }
        if let Some(prewhere) = &select.prewhere {
            self.validate_expr(prewhere, ctx)?;
        }
        if let Some(selection) = &select.selection {
            self.validate_expr(selection, ctx)?;
        }
        match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => {
                for expr in exprs {
                    self.validate_expr(expr, ctx)?;
                }
            }
            GroupByExpr::All(_) => {}
        }
        if let Some(having) = &select.having {
            self.validate_expr(having, ctx)?;
        }
        if let Some(qualify) = &select.qualify {
            self.validate_expr(qualify, ctx)?;
        }
        for expr in select
            .cluster_by
            .iter()
            .chain(select.distribute_by.iter())
            .chain(select.sort_by.iter())
        {
            self.validate_expr(expr, ctx)?;
        }
        Ok(())
    }

    pub(crate) fn validate_table_with_joins(
        &self,
        twj: &TableWithJoins,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        self.validate_table_factor(&twj.relation, ctx)?;
        ctx.joins += twj.joins.len();
        for join in &twj.joins {
            self.validate_table_factor(&join.relation, ctx)?;
            self.validate_join_operator(&join.join_operator, ctx)?;
        }
        Ok(())
    }

    pub(crate) fn validate_join_operator(
        &self,
        op: &JoinOperator,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match op {
            JoinOperator::Inner(c)
            | JoinOperator::LeftOuter(c)
            | JoinOperator::RightOuter(c)
            | JoinOperator::FullOuter(c)
            | JoinOperator::Semi(c)
            | JoinOperator::LeftSemi(c)
            | JoinOperator::RightSemi(c)
            | JoinOperator::Anti(c)
            | JoinOperator::LeftAnti(c)
            | JoinOperator::RightAnti(c) => self.validate_join_constraint(c, ctx),
            JoinOperator::CrossJoin | JoinOperator::CrossApply | JoinOperator::OuterApply => Ok(()),
            JoinOperator::AsOf {
                match_condition,
                constraint,
            } => {
                self.validate_expr(match_condition, ctx)?;
                self.validate_join_constraint(constraint, ctx)
            }
        }
    }

    pub(crate) fn validate_join_constraint(
        &self,
        constraint: &JoinConstraint,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match constraint {
            JoinConstraint::On(expr) => self.validate_expr(expr, ctx),
            JoinConstraint::Using(_) | JoinConstraint::Natural | JoinConstraint::None => Ok(()),
        }
    }

    pub(crate) fn validate_table_factor(
        &self,
        factor: &TableFactor,
        ctx: &mut ValidationContext,
    ) -> Result<(), ValidationBlocked> {
        match factor {
            TableFactor::Table { name, .. } => {
                if name.0.len() > 2 {
                    blocked!("Referencias de servidor/base de datos no permitidas: {name}");
                }
                ctx.tables.insert(name.to_string());
            }
            TableFactor::Derived { subquery, .. } => {
                // Early budget check: bail before recursing so deeply nested
                // derived tables fail fast instead of only at the final catch.
                ctx.subqueries += 1;
                if ctx.subqueries > self.policy.max_subqueries {
                    blocked!(
                        "Demasiadas subconsultas: máximo {}",
                        self.policy.max_subqueries
                    );
                }
                self.validate_query(subquery, ctx)?;
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.validate_table_with_joins(table_with_joins, ctx)?;
            }
            TableFactor::TableFunction { .. } => {
                blocked!("Funciones de tabla no permitidas");
            }
            _ => {
                blocked!("Tipo de tabla no permitido");
            }
        }
        Ok(())
    }
}
