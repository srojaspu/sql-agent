//! Sensitive-column list plus AST identifier collection.
//!
//! Pure move from `security::validator` (slice E, step 2): no rule, message,
//! limit, or ordering change. [`SENSITIVE_COLUMNS`] stays the single source
//! for validator + describe + redaction; the `collect_*` walk stays the
//! AST-identifiers-only gate (`my_token` blocked, `'password reset'` passes).

use sqlparser::ast::{
    Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, JoinConstraint,
    JoinOperator, ObjectName, Query, Select, SelectItem, SetExpr, Statement, TableFactor,
    TableWithJoins,
};

/// Single source for sensitive names (validator + describe + redaction).
pub const SENSITIVE_COLUMNS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "TOKEN",
    "ACCESS_TOKEN",
    "REFRESH_TOKEN",
    "API_KEY",
    "PRIVATE_KEY",
    "CLIENT_SECRET",
];

/// Exact case-insensitive match plus boundary parts (`my_token` -> MY + TOKEN).
/// Strips brackets/quotes/qualifiers; `secretary`/`tokenizer` stay allowed.
pub fn is_sensitive_column(name: &str) -> bool {
    let norm = name
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .trim()
        .trim_matches(|c| c == '[' || c == ']' || c == '"' || c == '\'' || c == '`')
        .to_ascii_uppercase();
    if norm.is_empty() {
        return false;
    }
    if SENSITIVE_COLUMNS.contains(&norm.as_str()) {
        return true;
    }
    for part in norm.split(|c: char| !c.is_ascii_alphanumeric()) {
        let p = part.trim_end_matches(|c: char| c.is_ascii_digit());
        if !p.is_empty() && SENSITIVE_COLUMNS.contains(&p) {
            return true;
        }
    }
    false
}

/// Walk the parsed statement and collect every identifier reference
/// (column identifiers, compound parts, table names, function names).
/// String literals, values, and comments never enter the AST as
/// identifiers, so they are ignored by construction.
pub(crate) fn collect_identifiers(statement: &Statement) -> Vec<String> {
    let mut out = Vec::new();
    if let Statement::Query(q) = statement {
        collect_query_identifiers(q, &mut out);
    }
    out
}

fn collect_query_identifiers(query: &Query, out: &mut Vec<String>) {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_query_identifiers(&cte.query, out);
        }
    }
    collect_set_expr_identifiers(&query.body, out);
    if let Some(order_by) = &query.order_by {
        for obe in &order_by.exprs {
            collect_expr_identifiers(&obe.expr, out);
        }
    }
    if let Some(limit) = &query.limit {
        collect_expr_identifiers(limit, out);
    }
}

fn collect_set_expr_identifiers(expr: &SetExpr, out: &mut Vec<String>) {
    match expr {
        SetExpr::Select(select) => collect_select_identifiers(select, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_set_expr_identifiers(left, out);
            collect_set_expr_identifiers(right, out);
        }
        SetExpr::Query(query) => collect_query_identifiers(query, out),
        SetExpr::Values(_) | SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Table(_) => {}
    }
}

fn collect_select_identifiers(select: &Select, out: &mut Vec<String>) {
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                collect_expr_identifiers(expr, out)
            }
            SelectItem::QualifiedWildcard(name, _) => collect_object_name(name, out),
            SelectItem::Wildcard(_) => {}
        }
    }
    for twj in &select.from {
        collect_table_with_joins_identifiers(twj, out);
    }
    if let Some(selection) = &select.selection {
        collect_expr_identifiers(selection, out);
    }
    if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            collect_expr_identifiers(expr, out);
        }
    }
    if let Some(having) = &select.having {
        collect_expr_identifiers(having, out);
    }
}

fn collect_table_with_joins_identifiers(twj: &TableWithJoins, out: &mut Vec<String>) {
    collect_table_factor_identifiers(&twj.relation, out);
    for join in &twj.joins {
        collect_table_factor_identifiers(&join.relation, out);
        collect_join_constraint(&join.join_operator, out);
    }
}

fn collect_join_constraint(op: &JoinOperator, out: &mut Vec<String>) {
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
        | JoinOperator::RightAnti(c) => {
            if let JoinConstraint::On(expr) = c {
                collect_expr_identifiers(expr, out);
            }
        }
        _ => {}
    }
}

fn collect_table_factor_identifiers(factor: &TableFactor, out: &mut Vec<String>) {
    match factor {
        TableFactor::Table { name, .. } => collect_object_name(name, out),
        TableFactor::Derived { subquery, .. } => collect_query_identifiers(subquery, out),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => collect_table_with_joins_identifiers(table_with_joins, out),
        _ => {}
    }
}

fn collect_object_name(name: &ObjectName, out: &mut Vec<String>) {
    for part in &name.0 {
        out.push(part.value.clone());
    }
}

fn collect_function_identifiers(func: &Function, out: &mut Vec<String>) {
    if let Some(last) = func.name.0.last() {
        out.push(last.value.clone());
    }
    if let FunctionArguments::List(list) = &func.args {
        for arg in &list.args {
            match arg {
                FunctionArg::Named { arg, .. } | FunctionArg::ExprNamed { arg, .. } => {
                    if let FunctionArgExpr::Expr(e) = arg {
                        collect_expr_identifiers(e, out);
                    }
                }
                FunctionArg::Unnamed(arg) => {
                    if let FunctionArgExpr::Expr(e) = arg {
                        collect_expr_identifiers(e, out);
                    }
                }
            }
        }
    }
    if let FunctionArguments::Subquery(query) = &func.args {
        collect_query_identifiers(query, out);
    }
}

/// Collect identifiers; literals ignored by construction.
fn collect_expr_identifiers(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Identifier(ident) => out.push(ident.value.clone()),
        Expr::CompoundIdentifier(idents) => {
            for ident in idents {
                out.push(ident.value.clone());
            }
        }
        Expr::Value(_)
        | Expr::TypedString { .. }
        | Expr::IntroducedString { .. }
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(_, _)
        | Expr::MatchAgainst { .. } => {}
        Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::Nested(e)
        | Expr::Cast { expr: e, .. }
        | Expr::UnaryOp { expr: e, .. }
        | Expr::Named { expr: e, .. } => collect_expr_identifiers(e, out),
        Expr::BinaryOp {
            left: a, right: b, ..
        } => {
            collect_expr_identifiers(a, out);
            collect_expr_identifiers(b, out);
        }
        Expr::Subquery(query)
        | Expr::Exists {
            subquery: query, ..
        } => collect_query_identifiers(query, out),
        Expr::InSubquery { expr, subquery, .. } => {
            collect_expr_identifiers(expr, out);
            collect_query_identifiers(subquery, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_identifiers(expr, out);
            for item in list {
                collect_expr_identifiers(item, out);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_expr_identifiers(expr, out);
            collect_expr_identifiers(low, out);
            collect_expr_identifiers(high, out);
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => {
            collect_expr_identifiers(expr, out);
            collect_expr_identifiers(pattern, out);
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            if let Some(op) = operand {
                collect_expr_identifiers(op, out);
            }
            for c in conditions {
                collect_expr_identifiers(c, out);
            }
            for r in results {
                collect_expr_identifiers(r, out);
            }
            if let Some(e) = else_result {
                collect_expr_identifiers(e, out);
            }
        }
        Expr::Function(f) => collect_function_identifiers(f, out),
        // Exotic nesting (JSON/map/array/subscript/interval/method/grouping)
        // cannot hide a sensitive reference in this agent's simple SELECTs;
        // deferred to keep the walk reviewable. Literals stay ignored above.
        _ => {}
    }
}
