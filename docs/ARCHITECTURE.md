# Arquitectura

```text
Usuario
  |
  v
Agent
  |
  +--> Ollama / Qwen3
  |       |
  |       +--> search_schema
  |       +--> describe_table
  |       +--> execute_read_query
  |
  +--> Dispatcher cerrado
          |
          v
      SqlValidator (AST)
          |
          v
      SqlServer
          |
          v
      Resultado limitado
          |
          v
        Ollama
          |
          v
      Respuesta final
```

## Principio clave

El modelo puede proponer una acción, pero no puede decidir qué acciones existen ni qué permisos tiene. El dispatcher solo acepta tres nombres de herramientas y `execute_read_query` siempre pasa por el validator.

## Observabilidad

`--verbose` muestra pasos, herramientas, validación y tiempos sin mostrar chain-of-thought privado.
