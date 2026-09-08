# ADR-005: Ranking module for schema discovery

## Status
Accepted

## Context
The agent needs to discover relevant tables/columns from user queries. The original implementation had fuzzy search logic scattered in `core.rs` with:
- Ad-hoc Levenshtein distance
- Singularization/pluralization heuristics
- Accent stripping
- AND/OR fallback logic

This logic was tightly coupled to the agent loop and difficult to test in isolation.

## Decision
Extract a pure `ranking` module with:
- **String normalization**: `normalize_term` (lowercase + accent stripping + singularization)
- **Levenshtein distance**: Standard implementation with early exit optimization
- **Two-phase ranking**: Exact match → prefix match → fuzzy match
- **Masked ranking**: Pre-filter by allowlist before ranking
- **Precomputation**: Normalized names computed once per schema cache load

```rust
pub fn normalize_term(s: &str) -> String;
pub fn strip_accents(s: &str) -> String;
pub fn singularize(s: &str) -> String;
pub fn levenshtein(a: &str, b: &str) -> usize;
pub fn filter_and_rank_tables(query: &str, tables: &[TableInfo], k: usize) -> Vec<TableInfo>;
pub fn search_with_fallback(query: &str, tables: &[TableInfo], k: usize) -> (Vec<TableInfo>, Option<TableInfo>, Vec<TableInfo>);
pub fn search_with_fallback_masked(...) -> ...;
```

## Consequences

### Positive
- **Pure functions**: All ranking logic is pure, easy to test
- **Performance**: Precomputation avoids repeated normalization
- **Testability**: 30+ unit tests covering edge cases
- **Reusability**: Can be used by other components (e.g., TUI)

### Negative
- **Heuristic limits**: Singularization is simplistic (ES-only rules)
- **Language bias**: Optimized for Spanish table names

## Alternatives Considered
1. **External fuzzy search library (tantivy, fst)**: Adds heavy dependency. Rejected - custom logic is simple and domain-specific.
2. **PostgreSQL pg_trgm**: Requires DB extension. Rejected - not portable.
3. **Keep in core.rs**: Simpler but untestable. Rejected.

## References
- [ranking.rs](../src/agent/ranking.rs)
- [ranking tests](../src/agent/ranking.rs)
- [Schema cache integration](../src/agent/memory.rs)