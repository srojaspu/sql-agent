# Modelo de seguridad v0.7

## Frontera de confianza

El LLM es un componente no confiable. El prompt nunca concede permisos.

El único componente autorizado a ejecutar SQL es `SqlServer::execute_read`, y el agente solo llega allí mediante `execute_read_query` o el fallback controlado para SQL textual. Antes de ejecutar se pasa por `SqlValidator`.

## Capas

1. Credenciales fuera del prompt.
2. Login SQL Server dedicado.
3. Verificación de permisos de escritura/administración.
4. TLS.
5. Dispatcher cerrado de herramientas.
6. Parser AST de SQL Server.
7. Un statement.
8. SELECT/CTE SELECT solamente.
9. Allowlist de tablas.
10. CTEs tratados como nombres virtuales.
11. Subconsultas/JOINs limitados.
12. Sin referencias de servidor/base de datos de 3/4 partes.
13. Sin table-valued functions.
14. Bloqueo de metadatos del sistema.
15. Bloqueo de funciones/palabras peligrosas.
16. Bloqueo de columnas sensibles.
17. Límites de filas/resultados.
18. Timeout y concurrencia.
19. Auditoría.

## Recomendación de SQL Server

La defensa más importante sigue siendo SQL Server:

```sql
GRANT SELECT ON OBJECT::dbo.entradaLote TO sql_agent_reader;
```

No otorgar `db_owner`, `sysadmin` ni permisos de escritura.

## Allowlist

En producción usar:

```text
ALLOWED_TABLES=dbo.entradaLote,dbo.producto,dbo.almacen
```

La allowlist se comprueba también para tablas usadas dentro de CTEs y subconsultas.

## TLS

`DATABASE_TRUST_CERT=true` permite certificados no confiables y debe considerarse una opción de desarrollo. En producción usar `false` y una cadena de confianza correcta.

## Auditoría

La auditoría registra eventos y request IDs. Nunca se registra la contraseña. `AUDIT_SQL=false` permite ocultar SQL si las consultas pueden contener información sensible.

## Limitaciones deliberadas

No se permite:

- INSERT/UPDATE/DELETE/MERGE.
- DDL.
- EXEC/EXECUTE.
- DBCC/BACKUP/RESTORE.
- xp_cmdshell y funciones equivalentes.
- OPENROWSET/OPENQUERY/OPENDATASOURCE.
- WAITFOR.
- Table-valued functions.
- Acceso a `sys.*` e `INFORMATION_SCHEMA` por defecto.
- Referencias de otras bases/servidores.
