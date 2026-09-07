//! Table ranking and fuzzy search helpers (pure domain logic).
//!
//! Extracted verbatim from `agent::core` (slice D, step 1): normalized-name
//! precomputation plus the AND→OR→did-you-mean ranking passes and the
//! string helpers they share. No behavior change.

use crate::database::TableInfo;

/// Precomputed normalized names for one cached table, built once per
/// `cached_schema` load so ranking never calls `normalize_term` per query.
#[derive(Clone, Debug)]
pub struct NormalizedEntry {
    pub norm_full: String,
    pub norm_table: String,
}

/// Precompute normalized (`schema.table` and `table`) names once per cache load.
/// Ranking helpers take this slice instead of normalizing per table per call.
pub fn precompute_normalized(tables: &[TableInfo]) -> Vec<NormalizedEntry> {
    tables
        .iter()
        .map(|t| {
            let full = format!("{}.{}", t.schema, t.table);
            NormalizedEntry {
                norm_full: normalize_term(&full),
                norm_table: normalize_term(&t.table),
            }
        })
        .collect()
}

/*
 * ====================================================================
 * SEARCH HARDENING HELPERS (P0/P1 grounding)
 * ====================================================================
 */

pub fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'Á' | 'À' | 'Ä' | 'Â' => 'A',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'É' | 'È' | 'Ë' | 'Ê' => 'E',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'Í' | 'Ì' | 'Ï' | 'Î' => 'I',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'Ó' | 'Ò' | 'Ö' | 'Ô' => 'O',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'Ú' | 'Ù' | 'Ü' | 'Û' => 'U',
            'ñ' => 'n',
            'Ñ' => 'N',
            'ç' => 'c',
            'Ç' => 'C',
            _ => c,
        })
        .collect()
}

pub fn singularize(s: &str) -> String {
    if s.len() > 3 && s.ends_with("es") {
        s[..s.len() - 2].to_string()
    } else if s.len() > 2 && s.ends_with('s') {
        s[..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = std::cmp::min(
                std::cmp::min(prev[j] + 1, curr[j - 1] + 1),
                prev[j - 1] + cost,
            );
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

pub fn normalize_term(s: &str) -> String {
    let lower = s.to_lowercase();
    let stripped = strip_accents(&lower);
    singularize(&stripped)
}

/// Pure ranking helper used by `search_schema` and tests.
/// Returns ranked matches after AND→OR fallback, truncated to `max_results`.
/// For empty query, returns first `max_results` tables (no ranking).
pub fn filter_and_rank_tables(
    query: &str,
    tables: &[TableInfo],
    max_results: usize,
) -> Vec<TableInfo> {
    let (ranked, _) = search_with_fallback(query, tables, max_results);
    ranked
}

/// Score helper shared by every ranking phase (single-term vs multi-term).
fn rank_score(terms: &[String], norm_query_joined: &str, norm_table: &str) -> usize {
    if terms.len() == 1 {
        levenshtein(norm_query_joined, norm_table)
    } else {
        terms
            .iter()
            .map(|term| levenshtein(term, norm_table))
            .min()
            .unwrap_or(usize::MAX)
    }
}

/// Core ranking over precomputed normalized names with an allowlist mask.
/// Returns (matched, suggestion, top_suggestions): the zero-hit full ranking is
/// computed ONCE here, so callers reuse `top_suggestions` instead of re-ranking.
/// Only matched/suggested entries are cloned (never the full table Vec).
/// `allowed[i] == false` skips `tables[i]`; `None` means every entry is allowed.
pub fn search_with_fallback_masked(
    query: &str,
    tables: &[TableInfo],
    norm: &[NormalizedEntry],
    allowed: Option<&[bool]>,
    max_results: usize,
) -> (Vec<TableInfo>, Option<TableInfo>, Vec<TableInfo>) {
    debug_assert_eq!(tables.len(), norm.len());
    let q = query.trim();
    if q.is_empty() {
        let ranked: Vec<TableInfo> = tables
            .iter()
            .enumerate()
            .filter(|(i, _)| allowed.is_none_or(|m| m[*i]))
            .take(max_results)
            .map(|(_, t)| t.clone())
            .collect();
        return (ranked, None, Vec::new());
    }
    let terms: Vec<String> = q.split_whitespace().map(normalize_term).collect();
    let norm_query_joined = terms.join(" ");
    let is_allowed = |i: usize| allowed.is_none_or(|m| m[i]);

    // AND phase (indices only; clone after truncate)
    let mut and_matches: Vec<(usize, usize)> = Vec::new();
    for (i, n) in norm.iter().enumerate() {
        if !is_allowed(i) {
            continue;
        }
        let all_contain = terms
            .iter()
            .all(|term| n.norm_full.contains(term) || n.norm_table.contains(term));
        if all_contain {
            and_matches.push((i, rank_score(&terms, &norm_query_joined, &n.norm_table)));
        }
    }
    if !and_matches.is_empty() {
        and_matches.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| tables[a.0].table.cmp(&tables[b.0].table))
        });
        let out: Vec<TableInfo> = and_matches
            .into_iter()
            .take(max_results)
            .map(|(i, _)| tables[i].clone())
            .collect();
        return (out, None, Vec::new());
    }

    // OR phase
    let mut or_matches: Vec<(usize, usize)> = Vec::new();
    for (i, n) in norm.iter().enumerate() {
        if !is_allowed(i) {
            continue;
        }
        let any_contain = terms
            .iter()
            .any(|term| n.norm_full.contains(term) || n.norm_table.contains(term));
        if any_contain {
            or_matches.push((i, rank_score(&terms, &norm_query_joined, &n.norm_table)));
        }
    }
    if !or_matches.is_empty() {
        or_matches.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| tables[a.0].table.cmp(&tables[b.0].table))
        });
        let out: Vec<TableInfo> = or_matches
            .into_iter()
            .take(max_results)
            .map(|(i, _)| tables[i].clone())
            .collect();
        return (out, None, Vec::new());
    }

    // Zero matches → Did-you-mean: single ranking pass yields both the closest
    // suggestion and the truncated top list, so callers never re-rank.
    let mut all_ranked: Vec<(usize, usize)> = norm
        .iter()
        .enumerate()
        .filter(|(i, _)| is_allowed(*i))
        .map(|(i, n)| (i, levenshtein(&norm_query_joined, &n.norm_table)))
        .collect();
    all_ranked.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| tables[a.0].table.cmp(&tables[b.0].table))
    });
    let suggestion = all_ranked.first().map(|(i, _)| tables[*i].clone());
    let top: Vec<TableInfo> = all_ranked
        .iter()
        .take(max_results)
        .map(|(i, _)| tables[*i].clone())
        .collect();
    // Return empty ranked but with suggestion; caller will format 0 + Did-you-mean
    (Vec::new(), suggestion, top)
}

/// Ranking over precomputed normalized names (single pass, no per-call normalize).
pub fn search_with_fallback_precomputed(
    query: &str,
    tables: &[TableInfo],
    norm: &[NormalizedEntry],
    max_results: usize,
) -> (Vec<TableInfo>, Option<TableInfo>) {
    let (matched, suggestion, _) =
        search_with_fallback_masked(query, tables, norm, None, max_results);
    (matched, suggestion)
}

/// Returns (ranked_matches, did_you_mean) where `did_you_mean` is Some(closest)
/// when no matches were found (for Did-you-mean suggestion). When query is empty,
/// `did_you_mean` is None and `ranked` is truncated list.
/// Compat wrapper: precomputes normalized names once, then delegates to the
/// masked core so ordering matches `search_with_fallback_precomputed` exactly.
pub fn search_with_fallback(
    query: &str,
    tables: &[TableInfo],
    max_results: usize,
) -> (Vec<TableInfo>, Option<TableInfo>) {
    let norm = precompute_normalized(tables);
    search_with_fallback_precomputed(query, tables, &norm, max_results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::TableInfo;

    fn make_tables(names: &[(&str, &str)]) -> Vec<TableInfo> {
        names
            .iter()
            .map(|(s, t)| TableInfo {
                schema: s.to_string(),
                table: t.to_string(),
                table_type: "BASE TABLE".to_string(),
            })
            .collect()
    }

    #[test]
    fn singularize_usuarios() {
        assert_eq!(singularize("usuarios"), "usuario");
    }

    #[test]
    fn singularize_roles() {
        assert_eq!(singularize("roles"), "rol");
    }

    #[test]
    fn singularize_meses() {
        // "meses" -> "mes" via es removal
        assert_eq!(singularize("meses"), "mes");
    }

    #[test]
    fn strip_accents_cancion() {
        assert_eq!(strip_accents("canción"), "cancion");
    }

    #[test]
    fn strip_accents_espanol() {
        assert_eq!(strip_accents("español"), "espanol");
    }

    #[test]
    fn levenshtein_kitten_sitting() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn levenshtein_usuarios_usuario() {
        // "usuarios" vs "usuario" distance 1 (extra s)
        assert_eq!(levenshtein("usuarios", "usuario"), 1);
    }

    #[test]
    fn normalize_term_usuarios() {
        assert_eq!(normalize_term("Usuarios"), "usuario");
    }

    #[test]
    fn normalize_term_with_accent() {
        assert_eq!(normalize_term("Canciones"), "cancion");
    }

    #[test]
    fn search_rank_usuarios_finds_usuario_first() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto"), ("dbo", "Pedido")]);
        let ranked = filter_and_rank_tables("usuarios", &tables, 20);
        assert!(!ranked.is_empty(), "should find at least one");
        assert_eq!(ranked[0].table, "Usuario");
    }

    #[test]
    fn search_or_fallback_finds_partial() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        // query "usu prod" AND would require both terms in same table -> 0, OR should find both
        let ranked = filter_and_rank_tables("usu prod", &tables, 20);
        // With OR fallback, should find both tables (each matches one term)
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn search_zero_returns_did_you_mean_candidates() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        let (ranked, did_you_mean) = search_with_fallback("xyz_noexiste", &tables, 20);
        assert!(ranked.is_empty(), "no direct match");
        assert!(did_you_mean.is_some(), "should suggest Did you mean");
        let dm = did_you_mean.unwrap();
        assert!(
            dm.table == "Usuario" || dm.table == "Producto",
            "Did you mean should be one of the candidates, got {}",
            dm.table
        );
    }

    #[test]
    fn search_truncates_to_max_k() {
        let tables: Vec<TableInfo> = (0..30)
            .map(|i| TableInfo {
                schema: "dbo".into(),
                table: format!("Tabla{i:02}"),
                table_type: "BASE TABLE".into(),
            })
            .collect();
        let ranked = filter_and_rank_tables("", &tables, 20);
        assert_eq!(ranked.len(), 20, "should truncate to MAX_SCHEMA_RESULTS 20");
    }

    // ===== P1a-7 ranking: same order/suggestion, precomputed + single pass =====
    #[test]
    fn precomputed_ranking_matches_legacy_order_and_suggestion() {
        let tables = make_tables(&[
            ("dbo", "Usuario"),
            ("dbo", "Producto"),
            ("dbo", "Pedido"),
            ("dbo", "UsuarioDireccion"),
        ]);
        for query in ["usuarios", "usu prod", "xyz_noexiste", "pedido", ""] {
            let (legacy_matched, legacy_sugg) = search_with_fallback(query, &tables, 20);
            let norm = precompute_normalized(&tables);
            let (pre_matched, pre_sugg) =
                search_with_fallback_precomputed(query, &tables, &norm, 20);
            let legacy_names: Vec<_> = legacy_matched
                .iter()
                .map(|t| format!("{}.{}", t.schema, t.table))
                .collect();
            let pre_names: Vec<_> = pre_matched
                .iter()
                .map(|t| format!("{}.{}", t.schema, t.table))
                .collect();
            assert_eq!(pre_names, legacy_names, "order must match for '{query}'");
            assert_eq!(
                pre_sugg.map(|t| t.table),
                legacy_sugg.map(|t| t.table),
                "suggestion must match for '{query}'"
            );
        }
    }

    #[test]
    fn masked_zero_hit_returns_suggestion_and_top_from_single_pass() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        let norm = precompute_normalized(&tables);
        let (matched, suggestion, top) =
            search_with_fallback_masked("xyz_noexiste", &tables, &norm, None, 20);
        assert!(matched.is_empty());
        let sugg = suggestion.expect("zero-hit must suggest");
        assert!(!top.is_empty(), "top reuses the same ranking pass");
        assert_eq!(top[0].table, sugg.table, "suggestion is top[0], no re-rank");
        // Mask filters without cloning an `allowed` Vec.
        let mask = vec![true, false];
        let (masked_matched, _, masked_top) =
            search_with_fallback_masked("", &tables, &norm, Some(&mask), 20);
        assert_eq!(masked_matched.len(), 1);
        assert_eq!(masked_matched[0].table, "Usuario");
        assert!(masked_top.is_empty(), "empty query has no suggestions");
    }
}
