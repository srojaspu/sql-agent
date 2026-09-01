$ErrorActionPreference = "Stop"

Write-Host "=== 1. Ollama ==="
ollama --version
ollama ps

Write-Host "`n=== 2. Rust ==="
rustc --version
cargo --version

Write-Host "`n=== 3. Build ==="
cargo check

Write-Host "`n=== 4. Tests ==="
cargo test

Write-Host "`n=== 5. DB ==="
cargo run -- --check-db

Write-Host "`n=== 6. Agent ==="
cargo run -- --verbose "¿Cuántos lote entradas se registraron este mes?"
