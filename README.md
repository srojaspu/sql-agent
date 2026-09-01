# SQL Agent Professional v0.7

Agente IA para consultar SQL Server en modo estrictamente read-only. Diseñado para Ollama/Qwen3 y para que el LLM **no tenga acceso directo a SQL Server**.

## Arquitectura

`Pregunta → Ollama → tools tipadas → SQL Validator AST → SQL Server → resultado → Ollama → respuesta`

Herramientas permitidas:

- `search_schema`
- `describe_table`
- `execute_read_query`

No se ejecuta ninguna herramienta cuyo nombre no esté en el dispatcher.

## Seguridad incluida

- Login SQL Server dedicado.
- Verificación en cada conexión de que el login no tenga permisos `INSERT/UPDATE/DELETE/ALTER/CONTROL` sobre la base.
- TLS con opción `DATABASE_TRUST_CERT`; usar `false` en producción con CA/certificado confiable.
- Parser AST de SQL Server (`sqlparser`) antes de ejecutar.
- Un solo statement.
- Solo `SELECT` y `WITH ... SELECT`.
- CTE permitidos sin permitir que escapen la allowlist.
- Subconsultas y JOINs limitados.
- Referencias de servidor/base de datos (3/4 partes) bloqueadas.
- Table-valued functions bloqueadas.
- Tablas del sistema bloqueadas por defecto.
- Columnas sensibles bloqueadas por nombres comunes.
- Comentarios SQL bloqueados.
- Límites de filas y tamaño de tool result.
- Timeout de conexión y consulta.
- Concurrencia limitada.
- Resultsets consumidos en streaming hasta `MAX_ROWS + 1`, evitando cargar millones de filas en memoria.
- Auditoría JSONL sin almacenar contraseña.
- `think=false` en Ollama y limpieza defensiva de `<think>`.
- El prompt no es una frontera de seguridad: el validator es la frontera.

## Instalación

```powershell
copy .env.example .env
```

Configura el login read-only. Para una tabla concreta:

```sql
CREATE USER sql_agent_reader FOR LOGIN sql_agent_reader;
GRANT SELECT ON OBJECT::dbo.entradaLote TO sql_agent_reader;
```

No uses `sysadmin`, `db_owner` ni permisos de escritura.

Luego:

```powershell
cargo check
cargo test
cargo run -- --check-db
cargo run -- --verbose "¿Cuántos lote entradas se registraron este mes?"
```

## Ollama

Verifica que Ollama esté activo y que el modelo exista:

```powershell
ollama list
ollama show qwen3:4b
```

Prueba directamente la API:

```powershell
Invoke-RestMethod `
  -Uri "http://localhost:11434/api/chat" `
  -Method Post `
  -ContentType "application/json" `
  -Body '{"model":"qwen3:4b","messages":[{"role":"user","content":"2+2"}],"stream":false,"think":false}'
```

## Ver el trabajo del agente

Con:

```powershell
cargo run -- --verbose "¿Cuántos lote entradas se registraron este mes?"
```

se muestran eventos de alto nivel:

```text
STEP 1/8
LLM → qwen3:4b
Tool calls: 1
  ↳ search_schema
  ✓ search_schema completado
STEP 2/8
Tool calls: 1
  ↳ describe_table
  ✓ describe_table completado
STEP 3/8
Tool calls: 1
  ↳ execute_read_query
SECURITY GATE → AST + allowlist
SQL permitido
SQL Server → ejecutando consulta read-only
...
```

No se imprime el razonamiento privado del modelo.

## Producción

1. `DATABASE_TRUST_CERT=false`.
2. `ALLOWED_TABLES` explícito.
3. Login con SELECT únicamente.
4. `AUDIT_SQL=false` si las consultas pueden contener datos sensibles.
5. Mantener Ollama en `127.0.0.1` si es local.
6. Si Ollama es remoto, usar HTTPS/VPN/red privada.
7. Revisar `logs/agent-audit.jsonl`.

## v0.7.2

- Corrige el test de límite de JOINs: `max_joins` es inclusivo. Con `max_joins=5`, cinco JOINs son válidos y seis son rechazados.
- Añade pruebas explícitas del límite (boundary tests).
- Sin cambios a la política de seguridad de ejecución.
