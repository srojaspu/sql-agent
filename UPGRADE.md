# Upgrade Guide - SQL Agent 0.7 → 0.8

## ¿Qué Cambió?

Tu agente ahora **responde preguntas correctamente** en lugar de solo describir estructura.

### Antes (0.7)
```
👤 ¿Cuántos roles tiene la tabla rol?
━━━━━━━━ STEP 1/8 ━━━━━━━━
🔎 search_schema → 1 tabla
📐 describe_table → 4 columnas
━━━━━━━━ RESULTADO ━━━━━━━━
La tabla `rol` tiene estas columnas:
  - id (int)
  - descripcion (varchar)
  - codigo (varchar)
  - estado (bit)
❌ NO hay información adicional (SIN DATOS REALES)
```

### Después (0.8)
```
👤 ¿Cuántos roles tiene la tabla rol?
━━━━━━━━ STEP 1/8 ━━━━━━━━
🔧 Tool calls: 2
  ↳ search_schema
  ↳ describe_table
━━━━━━━━ STEP 2/8 ━━━━━━━━
🔧 Tool calls: 1
  ↳ execute_read_query
📊 5 fila(s)
━━━━━━━━ RESULTADO ━━━━━━━━
La tabla `rol` tiene 5 roles:
  1. Administrador (cod: ADM)
  2. Usuario (cod: USR)
  3. Lectura (cod: READ)
  ...
✅ DATOS REALES DE LA BASE
```

## ¿Por Qué Funcionaba Antes?

El modelo pequeño (Qwen 1.7B) interpretaba:
- "describe_table terminado" = "tengo suficiente información" 
- NO sabía que necesitaba un TERCER paso (execute_read_query)

## ¿Cómo Se Arregló?

### 1. Prompt Rediseñado
Ahora incluye ejemplo EXACTO de tu caso de uso:

```
=== EJEMPLO PARA PREGUNTA: "cuántos roles tiene la tabla rol" ===
1. search_schema(query: "rol") → encuentra dbo.rol
2. describe_table(table: "rol") → ve columnas
3. execute_read_query(sql: "SELECT COUNT(*) FROM dbo.rol") → obtiene DATOS
4. Responde con números REALES
```

### 2. Validador de Completitud
El agente ahora verifica: "¿Mi respuesta tiene DATOS o solo estructura?"

Si detecta que falta, AUTO-ejecuta la query:
```rust
if self.should_force_data_query(&response, &tool_history, step) {
    println!("⚠️ Respuesta incompleta detectada. Forzando query...");
    // Escalada automática a execute_read_query
}
```

### 3. Output Mejorado
Cada herramienta ahora indica CLARAMENTE el siguiente paso:

```
✓ ESTRUCTURA DE dbo.rol
COLUMNAS:
  • id (int) [NO NULO]
  • descripcion (varchar) [NO NULO]

→ IMPORTANTE: Esta es solo la ESTRUCTURA. 
Para obtener los DATOS REALES (filas, valores), usa execute_read_query
```

## Qué Tipo de Preguntas Mejoraron

### ✅ Preguntas que AHORA funcionan bien

```sql
"¿Cuántos usuarios hay?"
→ SELECT COUNT(*) FROM usuarios

"¿Cuáles son los estados activos?"
→ SELECT DISTINCT estado FROM usuarios WHERE activo = 1

"¿Listar todos los roles?"
→ SELECT * FROM rol

"¿Qué tablas hay?"
→ search_schema("tabla")

"¿Cuáles usuarios están registrados?"
→ SELECT nombre FROM usuarios
```

### ✅ Preguntas que SIGUEN siendo eficientes

```sql
"¿Qué columnas tiene la tabla usuario?"
→ describe_table("usuario")
→ NO ejecuta query innecesaria ✓

"¿La tabla 'historia' existe?"
→ search_schema("historia")
→ Responde directo ✓
```

## Installation / Upgrade

Solo necesitas compilar la nueva versión:

```powershell
cd c:\Projects\Rust\sql-agent
cargo build --release
```

El binario está en: `target\release\sql-agent.exe`

### ¿Config Changes?
**NO** - Todos los valores de `.env` son compatibles.

No necesitas cambiar nada en:
- DATABASE_* (host, puerto, usuario)
- OLLAMA_* (modelo, URL)
- MAX_* (steps, rows, joins)

## Troubleshooting

### Problema: "El login SQL tiene permisos de escritura"
```
Error: El login SQL tiene permisos de escritura/administración
```

**Solución**: Usar un usuario de solo lectura
```sql
-- En SQL Server
CREATE USER sql_agent_reader FOR LOGIN sql_agent_reader;
GRANT SELECT ON OBJECT::dbo.rol TO sql_agent_reader;
GRANT SELECT ON OBJECT::dbo.usuario TO sql_agent_reader;
```

Luego actualiza `.env`:
```
DATABASE_USER=sql_agent_reader
DATABASE_PASSWORD=tu_contraseña
```

### Problema: "Respuesta vacía/timeout"
- Revisa que Ollama está corriendo: `ollama serve qwen3:1.7b`
- Verifica que el host BD es accesible: `ping 172.0.1.156`
- Intenta con `.verbose` para ver logs: `cargo run -- --verbose "pregunta"`

### Problema: Ejecuta too many steps sin responder
- Indica que el modelo se confunde
- Intenta una pregunta más específica
- O sube a `qwen3:4b` (lento pero mejor reasoning)

## Performance Notes

### Velocidad (Qwen 1.7B)
- Preguntas de estructura: ~1-2s
- Preguntas con datos: ~2-4s  
- Sin overhead visible

### Consumo de memoria
- Sin cambio vs 0.7
- Historial de tools es Vec<String> pequeño

### Concurrencia
- Max 1 query por defecto (ver `MAX_CONCURRENT_QUERIES`)
- Aumenta a 4-8 si tienes BD de pruebas sin contencion

## Ejemplos de Uso

### Ejemplo 1: Contar registros
```bash
$ cargo run -- "¿cuántos usuarios registrados hay en la tabla usuario?"

━━━━━━━━ STEP 1/8 ━━━━━━━━
🔧 Tool calls: 1
   ↳ search_schema
🔎 search_schema → 1 coincidencias

━━━━━━━━ STEP 2/8 ━━━━━━━━
🔧 Tool calls: 2
   ↳ describe_table
   ↳ execute_read_query

━━━━━━━━ RESULTADO ━━━━━━━━
Hay 127 usuarios registrados en la tabla usuario.
```

### Ejemplo 2: Listar valores
```bash
$ cargo run -- "¿cuáles son los diferentes estados de usuarios?"

━━━━━━━━ STEP 1/8 ━━━━━━━━
🔧 Tool calls: 1
   ↳ search_schema

━━━━━━━━ STEP 2/8 ━━━━━━━━
🔧 Tool calls: 2
   ↳ describe_table
   ↳ execute_read_query

━━━━━━━━ RESULTADO ━━━━━━━━
Los estados de usuarios son:
  • Activo
  • Inactivo
  • Suspendido
  • Pendiente
```

### Ejemplo 3: Estructura (no ejecuta query innecesaria)
```bash
$ cargo run -- "¿qué campos tiene la tabla usuario?"

━━━━━━━━ STEP 1/8 ━━━━━━━━
🔧 Tool calls: 1
   ↳ describe_table

━━━━━━━━ RESULTADO ━━━━━━━━
La tabla `usuario` tiene los siguientes campos:
  • id (int)
  • nombre (varchar)
  • email (varchar)
  • estado (varchar)
```

## FAQ

**P: ¿Por qué a veces ejecuta 3 pasos si podría hacerlo en 2?**
R: Es más seguro. El validador prefiere asegurar datos reales que asumir.

**P: ¿Puedo volver a 0.7?**
R: Sí, pero no hay razón. 0.8 es 100% compatible y responde mejor.

**P: ¿Quién decide si usar describe_table vs execute_read_query?**
R: El modelo, guiado por el prompt mejorado. Si la pregunta pide "cuántos/cuáles/listar", usa query.

**P: ¿Soporta transacciones o procedimientos?**
R: No. Es READ-ONLY por diseño. Solo SELECT y WITH...SELECT.

**P: ¿Puedo usar este agente en producción?**
R: Sí, con:
- Usuario SQL de solo lectura
- TLS en BD (o DATABASE_TRUST_CERT=false en dev)
- Auditoría habilitada
- Timeout configurado apropiadamente

## Reporting Issues

Si encuentras un problema:

1. Ejecuta con `--verbose` y captura el output
2. Verifica que el usuario BD tiene permisos correctos
3. Reporta con: tipo de pregunta, output completo, versión de Qwen

---

**Happy querying! 🚀**
