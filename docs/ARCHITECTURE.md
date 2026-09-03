# Arquitectura

```text
Usuario ──→ main.rs (clap Args {verbose,check_db,question,no_tui})
          ├─ one-shot/pipe/non-TTY → Agent::run(question) ─→ Ollama ↔ tools
          └─ TUI ─→ TuiApp {AppState, Event(mpsc), UI frames}
                       │  crossterm events (input/scroll/cmd) ─→ commands
                       └─→ Agent::run_with_history(&mut Session, &str) ─→ SqlServer/SchemaCache/Validator/LLM
                               ↕ SchemaMemory (synonyms, last_used, ttl 300s)
                               ↕ history.jsonl (~/.sql-agent/history.jsonl → ./logs/chat-history.jsonl)
```

Flujo original (one-shot) preservado: `Pregunta → Ollama → tools tipadas → SQL Validator AST → SQL Server → resultado → Ollama → respuesta`.

Herramientas: `search_schema` (hardened: normalize singularize accent-strip OR-fallback Levenshtein top-K 20 + Did-you-mean) → `describe_table` → `execute_read_query`.

## Principio clave

El modelo puede proponer una acción, pero no puede decidir qué acciones existen ni qué permisos tiene. El dispatcher solo acepta tres nombres de herramientas y `execute_read_query` siempre pasa por el validator (`SqlValidator` es la única frontera; el prompt no lo es).

Grounding invariante: `search_schema` 0 nunca inventa `dbo.*`; si 0, retorna `0 + top-K + Did you mean` y bloquea `describe/execute` hallucinated; `Invalid object name` re-inyecta hasta 17 candidatos y continúa dentro de `MAX_STEPS`.

## TUI híbrida

- Entrada: `cargo run` bare → TUI si `stdout.is_terminal() && stdin.is_terminal()`; `--no-tui` fuerza one-shot; pipe/non-TTY hace fallback plain sin raw mode.
- Guard: `TerminalGuard` `Drop` + `OnceLock` panic hook restaura `LeaveAlternateScreen` + `disable_raw_mode` (best-effort, Windows safe).
- AppState: `Session {id, messages, schema_memory}`, input, messages `ChatLine`, scroll_offset, status, current_tool, step, is_loading, show_help, history_visible; métodos `push/clear/scroll/handle_event/handle_command/visible_messages/channel`.
- Eventos: `tokio::sync::mpsc` `AppEvent {Input, Step, Tool, Done, Error, Quit}` Agent → UI; `crossterm::event::poll` no-bloqueante cada 50ms.
- UI: `ratatui` 0.29 + `crossterm` 0.28 panes chat 70% + input 15% + status 15% (borders, wrap, colores); comandos `/clear` `/history` `/tables` `/describe <t>` `/refresh` `/quit` `/help`; TestBackend para panes.
- Integración: `Arc<Agent>` + `Session` clone para `tokio::spawn` `run_with_history`; `/tables` via `tui_list_tables` (cached+allowlist), `/describe` via `tui_describe`, `/refresh` via `refresh_cache` + `schema_memory.clear()`.

## Memoria y sesión

- `Session` caps 40 msgs / 30k chars (trunca `tool` más antiguos primero, luego summarize).
- `build_history_context` + `build_messages_with_history` inyectan historial + `schema_memory` válida (TTL 300s) + hint anafórico (`y de esos`/`de esos`) al system prompt.
- Persistencia JSONL `~/.sql-agent/history.jsonl` append-only, rotación 10k líneas o 5MB (mantiene 5000), redacción `[REDACTED sensitive]`, líneas corruptas omitidas, `USERPROFILE` fallback sin crate `dirs`.
- `SchemaMemoryEntry {table, columns, synonyms, last_used, hit_count, ttl_seconds}` con `upsert_schema_memory` y `invalidate_all`; TTL compartido con `SchemaCache`.

## Config y seguridad

- `Config::from_map` canónica, aliases `BLOCKED_TABLES`/`BLOCKED_COLUMNS` → `ALLOWED_TABLES` y `MAX_AGENT_STEPS` → `MAX_STEPS` con `tracing::warn`, fail-fast en `validate_no_unknown`, drift `find_drifted` vs `INFORMATION_SCHEMA.TABLES`.
- `SqlValidator` (SELECT-only, single-stmt, allowlist, join/subquery caps, comment/system block 3/4-part, TVF block) intacto; `max_result_chars` + `max_tool_result_chars` en memoria+audit.

## Observabilidad

`--verbose` muestra pasos, herramientas, validación y tiempos sin mostrar chain-of-thought privado. `tracing_subscriber::fmt().without_time()` sin banner `time`.

## Tests de referencia

- `tests/schema_grounding.rs` 32 tests: singularize/strip_accents/levenshtein/normalize + `filter_and_rank_tables`/`search_with_fallback` + K trunc + 0 Did-you-mean + acentos/plurales (usuarios, administradores, Canciones, clientes).
- `tests/anti_hall.rs` 14 tests: harness 0-nunca-dbo.* + Invalid-reinject 17 + grounding bajo MAX_STEPS + prompt “NO inventes”+“Did you mean”.
- `tests/security_tests.rs` 31 tests: allowlist/CTE/join/subquery siguen bloqueados, alias no amplía, anti-hall no bypass validator + clippy/fmt.
