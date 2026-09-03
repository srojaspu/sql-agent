use anyhow::{bail, Result};
use sqlparser::{
    ast::{
        Distinct, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentClause,
        FunctionArguments, GroupByExpr, HavingBound, JoinConstraint, JoinOperator, JsonPathElem,
        ObjectName, Query, Select, SelectItem, SetExpr, Statement, Subscript, TableFactor,
        TableWithJoins, TopQuantity, WindowType,
    },
    dialect::MsSqlDialect,
    parser::Parser,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct SecurityPolicy {
    pub max_sql_length: usize,
    pub allowed_tables: Vec<String>,
    pub block_sensitive_columns: bool,
    pub block_comments: bool,
    pub allow_cte: bool,
    pub allow_system_tables: bool,
    pub max_joins: usize,
    pub max_subqueries: usize,
}

pub struct SqlValidator {
    policy: SecurityPolicy,
}

impl SqlValidator {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self { policy }
    }

    pub fn validate(&self, sql: &str) -> Result<()> {
        let sql = sql.trim();
        if sql.is_empty() {
            bail!("SQL vacío");
        }
        if sql.len() > self.policy.max_sql_length {
            bail!("SQL supera MAX_SQL_LENGTH");
        }
        if self.policy.block_comments
            && (sql.contains("--") || sql.contains("/*") || sql.contains("*/"))
        {
            bail!("Comentarios SQL no permitidos");
        }

        let statements = Parser::parse_sql(&MsSqlDialect {}, sql)
            .map_err(|e| anyhow::anyhow!("SQL inválido: {e}"))?;
        if statements.len() != 1 {
            bail!("Solo se permite un statement");
        }

        let upper = sql.to_ascii_uppercase();
        for word in [
            "XP_CMDSHELL",
            "SP_EXECUTESQL",
            "OPENROWSET",
            "OPENQUERY",
            "OPENDATASOURCE",
            "BULK",
            "BACKUP",
            "RESTORE",
            "DBCC",
            "WAITFOR",
            "SHUTDOWN",
            "GRANT",
            "REVOKE",
            "DENY",
            "KILL",
        ] {
            if contains_word(&upper, word) {
                bail!("Operación/función bloqueada: {word}");
            }
        }

        let statement = &statements[0];
        let query = match statement {
            Statement::Query(q) => q,
            _ => bail!("Solo se permite SELECT/CTE SELECT"),
        };

        let mut ctx = ValidationContext::default();
        self.validate_query(query, &mut ctx)?;

        if ctx.joins > self.policy.max_joins {
            bail!("Demasiados JOINs: máximo {}", self.policy.max_joins);
        }
        if ctx.subqueries > self.policy.max_subqueries {
            bail!(
                "Demasiadas subconsultas: máximo {}",
                self.policy.max_subqueries
            );
        }

        if !self.policy.allow_system_tables {
            for table in &ctx.tables {
                let n = normalize_table(table);
                if n.starts_with("sys.")
                    || n.contains("information_schema")
                    || n.starts_with("master.")
                {
                    bail!("Acceso a metadatos/sistema no permitido: {table}");
                }
            }
        }

        if !self.policy.allowed_tables.is_empty() {
            let allowed: HashSet<String> = self
                .policy
                .allowed_tables
                .iter()
                .map(|s| normalize_table(s))
                .collect();
            for table in &ctx.tables {
                if ctx.ctes.contains(&normalize_table(table)) {
                    continue;
                }
                if !allowed.contains(&normalize_table(table)) {
                    bail!("Tabla no permitida: {table}");
                }
            }
        }

        if self.policy.block_sensitive_columns {
            for word in [
                "PASSWORD",
                "PASSWD",
                "SECRET",
                "TOKEN",
                "ACCESS_TOKEN",
                "REFRESH_TOKEN",
                "API_KEY",
                "PRIVATE_KEY",
                "CLIENT_SECRET",
            ] {
                if contains_word(&sql.to_ascii_uppercase(), word) {
                    bail!("Columna sensible bloqueada: {word}");
                }
            }
        }

        Ok(())
    }

    fn validate_query(&self, query: &Query, ctx: &mut ValidationContext) -> Result<()> {
        if let Some(with) = &query.with {
            if !self.policy.allow_cte {
                bail!("CTE/WITH no permitido");
            }
            for cte in &with.cte_tables {
                ctx.ctes.insert(normalize_table(&cte.alias.name.value));
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
    fn validate_nested_query(&self, query: &Query, ctx: &mut ValidationContext) -> Result<()> {
        ctx.subqueries += 1;
        if ctx.subqueries > self.policy.max_subqueries {
            bail!(
                "Demasiadas subconsultas: máximo {}",
                self.policy.max_subqueries
            );
        }
        self.validate_query(query, ctx)
    }

    fn validate_set_expr(&self, expr: &SetExpr, ctx: &mut ValidationContext) -> Result<()> {
        match expr {
            SetExpr::Select(select) => self.validate_select(select, ctx),
            SetExpr::SetOperation { left, right, .. } => {
                // Every UNION/EXCEPT/INTERSECT branch must be read-only.
                self.validate_set_expr(left, ctx)?;
                self.validate_set_expr(right, ctx)
            }
            SetExpr::Query(query) => self.validate_nested_query(query, ctx),
            SetExpr::Values(_) | SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Table(_) => {
                bail!("Expresión SQL no permitida")
            }
        }
    }

    fn validate_select(&self, select: &Select, ctx: &mut ValidationContext) -> Result<()> {
        // SELECT INTO creates a table: it is a write, never read-only.
        if select.into.is_some() {
            bail!("SELECT INTO no permitido");
        }
        // Without FROM there is no allowlist scope to resolve.
        if select.from.is_empty() {
            bail!("SELECT sin FROM no permitido");
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

    /// Every wildcard in the projection must resolve to a known scope, and
    /// every scoped base table must be allowlisted (CTEs excluded, mirroring
    /// the global allowlist check).
    fn check_wildcard_scope(
        &self,
        projection: &[SelectItem],
        scope: &Scope,
        ctx: &ValidationContext,
    ) -> Result<()> {
        let mut has_wildcard = false;
        for item in projection {
            match item {
                SelectItem::Wildcard(_) => has_wildcard = true,
                SelectItem::QualifiedWildcard(name, _) => {
                    has_wildcard = true;
                    if scope.resolve(name).is_none() {
                        bail!("Wildcard con alcance desconocido: {name}");
                    }
                }
                SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. } => {}
            }
        }
        if has_wildcard && !self.policy.allowed_tables.is_empty() {
            let allowed: HashSet<String> = self
                .policy
                .allowed_tables
                .iter()
                .map(|s| normalize_table(s))
                .collect();
            for table in &scope.tables {
                if ctx.ctes.contains(&normalize_table(table)) {
                    continue;
                }
                if !allowed.contains(&normalize_table(table)) {
                    bail!("Tabla no permitida: {table}");
                }
            }
        }
        Ok(())
    }

    fn validate_table_with_joins(
        &self,
        twj: &TableWithJoins,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
        self.validate_table_factor(&twj.relation, ctx)?;
        ctx.joins += twj.joins.len();
        for join in &twj.joins {
            self.validate_table_factor(&join.relation, ctx)?;
            self.validate_join_operator(&join.join_operator, ctx)?;
        }
        Ok(())
    }

    fn validate_join_operator(
        &self,
        op: &JoinOperator,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
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

    fn validate_join_constraint(
        &self,
        constraint: &JoinConstraint,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
        match constraint {
            JoinConstraint::On(expr) => self.validate_expr(expr, ctx),
            JoinConstraint::Using(_) | JoinConstraint::Natural | JoinConstraint::None => Ok(()),
        }
    }

    /// Recursively walk an expression so subqueries hidden in projection,
    /// WHERE/HAVING, GROUP/ORDER BY, LIMIT/OFFSET, CASE, function arguments,
    /// or JOIN conditions re-enter query validation under budget.
    fn validate_expr(&self, expr: &Expr, ctx: &mut ValidationContext) -> Result<()> {
        match expr {
            // Leaves: no nested expressions or queries.
            Expr::Identifier(ident) => {
                if is_at_variable(&ident.value) {
                    bail!("Variable de sistema bloqueada: {}", ident.value);
                }
                Ok(())
            }
            Expr::CompoundIdentifier(idents) => {
                for ident in idents {
                    if is_at_variable(&ident.value) {
                        bail!("Variable de sistema bloqueada: {}", ident.value);
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
                left: a,
                right: b,
                ..
            }
            | Expr::AnyOp {
                left: a,
                right: b,
                ..
            }
            | Expr::AllOp {
                left: a,
                right: b,
                ..
            } => {
                self.validate_expr(a, ctx)?;
                self.validate_expr(b, ctx)
            }
            // Subqueries re-enter query validation under budget.
            Expr::Subquery(query) | Expr::Exists { subquery: query, .. } => {
                self.validate_nested_query(query, ctx)
            }
            Expr::InSubquery {
                expr,
                subquery,
                ..
            } => {
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
                expr,
                array_expr,
                ..
            } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(array_expr, ctx)
            }
            Expr::Between {
                expr,
                low,
                high,
                ..
            } => {
                self.validate_expr(expr, ctx)?;
                self.validate_expr(low, ctx)?;
                self.validate_expr(high, ctx)
            }
            Expr::Like {
                expr, pattern, ..
            }
            | Expr::ILike {
                expr, pattern, ..
            }
            | Expr::SimilarTo {
                expr, pattern, ..
            }
            | Expr::RLike {
                expr, pattern, ..
            } => {
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

    fn validate_function(&self, func: &Function, ctx: &mut ValidationContext) -> Result<()> {
        let name = func.name.0.last().map(|i| i.value.as_str()).unwrap_or("");
        if is_blocked_system_func(name) {
            bail!("Función de sistema bloqueada: {name}");
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

    fn validate_function_args(
        &self,
        args: &FunctionArguments,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
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

    fn validate_function_arg_expr(
        &self,
        arg: &FunctionArgExpr,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
        match arg {
            FunctionArgExpr::Expr(e) => self.validate_expr(e, ctx),
            FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_) => Ok(()),
        }
    }

    fn validate_table_factor(
        &self,
        factor: &TableFactor,
        ctx: &mut ValidationContext,
    ) -> Result<()> {
        match factor {
            TableFactor::Table { name, .. } => {
                if name.0.len() > 2 {
                    bail!("Referencias de servidor/base de datos no permitidas: {name}");
                }
                ctx.tables.insert(name.to_string());
            }
            TableFactor::Derived { subquery, .. } => {
                ctx.subqueries += 1;
                self.validate_query(subquery, ctx)?;
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.validate_table_with_joins(table_with_joins, ctx)?;
            }
            TableFactor::TableFunction { .. } => {
                bail!("Funciones de tabla no permitidas");
            }
            _ => {
                bail!("Tipo de tabla no permitido");
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct ValidationContext {
    tables: HashSet<String>,
    ctes: HashSet<String>,
    joins: usize,
    subqueries: usize,
}

/// Alias-aware resolution of the tables visible to one SELECT scope.
/// Built from FROM plus JOIN relations so `*` and `alias.*` can be
/// scoped to known tables before the allowlist check runs.
#[derive(Default)]
struct Scope {
    /// Normalized base table names visible in this scope.
    tables: Vec<String>,
    /// Normalized qualifier (alias, full name, or short name) to the base
    /// table it refers to. `None` marks a derived table or CTE alias, which
    /// has no base table of its own.
    qualifiers: HashMap<String, Option<String>>,
}

impl Scope {
    fn collect(from: &[TableWithJoins]) -> Self {
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
                let norm = normalize_table(&base);
                if !self.tables.iter().any(|t| normalize_table(t) == norm) {
                    self.tables.push(base.clone());
                }
                self.qualifiers
                    .entry(norm)
                    .or_insert_with(|| Some(base.clone()));
                if let Some(short) = name.0.last() {
                    self.qualifiers
                        .entry(normalize_table(&short.value))
                        .or_insert_with(|| Some(base.clone()));
                }
                if let Some(a) = alias {
                    self.qualifiers
                        .entry(normalize_table(&a.name.value))
                        .or_insert_with(|| Some(base));
                }
            }
            TableFactor::Derived {
                alias: Some(a), ..
            } => {
                self.qualifiers
                    .entry(normalize_table(&a.name.value))
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
        if let Some(r) = self.qualifiers.get(&normalize_table(&name.to_string())) {
            return Some(r.clone());
        }
        if let Some(last) = name.0.last() {
            if let Some(r) = self.qualifiers.get(&normalize_table(&last.value)) {
                return Some(r.clone());
            }
        }
        if let Some(first) = name.0.first() {
            if let Some(r) = self.qualifiers.get(&normalize_table(&first.value)) {
                return Some(r.clone());
            }
        }
        None
    }
}

fn normalize_table(s: &str) -> String {
    s.replace('[', "")
        .replace(']', "")
        .replace('"', "")
        .to_ascii_lowercase()
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

fn contains_word(sql: &str, word: &str) -> bool {
    sql.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|p| p.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v() -> SqlValidator {
        SqlValidator::new(SecurityPolicy {
            max_sql_length: 10_000,
            allowed_tables: vec!["dbo.entradaLote".into()],
            block_sensitive_columns: true,
            block_comments: true,
            allow_cte: true,
            allow_system_tables: false,
            max_joins: 5,
            max_subqueries: 5,
        })
    }
    #[test]
    fn select_ok() {
        assert!(v().validate("SELECT TOP 10 * FROM dbo.entradaLote").is_ok());
    }
    #[test]
    fn max_joins_boundary_allowed() {
        let sql = "SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON 1=1 JOIN dbo.entradaLote c ON 1=1 JOIN dbo.entradaLote d ON 1=1 JOIN dbo.entradaLote e ON 1=1 JOIN dbo.entradaLote f ON 1=1";
        assert!(v().validate(sql).is_ok());
    }
    #[test]
    fn max_joins_exceeded() {
        let sql = "SELECT * FROM dbo.entradaLote a JOIN dbo.entradaLote b ON 1=1 JOIN dbo.entradaLote c ON 1=1 JOIN dbo.entradaLote d ON 1=1 JOIN dbo.entradaLote e ON 1=1 JOIN dbo.entradaLote f ON 1=1 JOIN dbo.entradaLote g ON 1=1";
        assert!(v().validate(sql).is_err());
    }
    #[test]
    fn delete_blocked() {
        assert!(v().validate("DELETE FROM dbo.entradaLote").is_err());
    }
    #[test]
    fn update_blocked() {
        assert!(v().validate("UPDATE dbo.entradaLote SET x=1").is_err());
    }
    #[test]
    fn insert_blocked() {
        assert!(v()
            .validate("INSERT INTO dbo.entradaLote VALUES (1)")
            .is_err());
    }
    #[test]
    fn drop_blocked() {
        assert!(v().validate("DROP TABLE dbo.entradaLote").is_err());
    }
    #[test]
    fn multi_statement_blocked() {
        assert!(v().validate("SELECT 1; SELECT 2").is_err());
    }
    #[test]
    fn comments_blocked() {
        assert!(v().validate("SELECT 1 -- DELETE").is_err());
    }
    #[test]
    fn sensitive_column_blocked() {
        assert!(v()
            .validate("SELECT password FROM dbo.entradaLote")
            .is_err());
    }
    #[test]
    fn unauthorized_table_blocked() {
        assert!(v().validate("SELECT * FROM dbo.usuarios_secretos").is_err());
    }
    #[test]
    fn cte_allowed() {
        assert!(v()
            .validate("WITH x AS (SELECT TOP 10 * FROM dbo.entradaLote) SELECT * FROM x")
            .is_ok());
    }
    #[test]
    fn cte_cannot_escape_allowlist() {
        assert!(v()
            .validate("WITH x AS (SELECT * FROM dbo.usuarios_secretos) SELECT * FROM x")
            .is_err());
    }
    #[test]
    fn subquery_allowlist() {
        assert!(v()
            .validate("SELECT * FROM dbo.entradaLote WHERE id IN (SELECT id FROM dbo.entradaLote)")
            .is_ok());
    }
    #[test]
    fn system_blocked() {
        assert!(v().validate("SELECT * FROM sys.objects").is_err());
    }
    #[test]
    fn system_func_via_from_blocked() {
        assert!(v()
            .validate("SELECT SUSER_SNAME() FROM dbo.entradaLote")
            .is_err());
        assert!(v().validate("SELECT HOST_NAME() FROM dbo.entradaLote").is_err());
        assert!(v().validate("SELECT DB_NAME() FROM dbo.entradaLote").is_err());
        assert!(v().validate("SELECT GETDATE() FROM dbo.entradaLote").is_err());
        assert!(v()
            .validate("SELECT @@VERSION FROM dbo.entradaLote")
            .is_err());
    }
    #[test]
    fn normal_select_with_allowed_func_passes() {
        assert!(v()
            .validate("SELECT COUNT(*) FROM dbo.entradaLote")
            .is_ok());
    }
}
