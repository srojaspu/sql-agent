$ErrorActionPreference = "Stop"

Write-Host "=== OLLAMA ==="
ollama --version

Write-Host "`n=== MODELOS ==="
ollama list

Write-Host "`n=== PROCESOS ==="
ollama ps

$model = if ($env:OLLAMA_MODEL) { $env:OLLAMA_MODEL } else { "qwen3:4b" }

Write-Host "`n=== API /api/chat ==="

$body = @{
  model = $model
  messages = @(
    @{
      role = "user"
      content = "Responde solamente OK"
    }
  )
  stream = $false
  think = $false
  options = @{
    temperature = 0
  }
} | ConvertTo-Json -Depth 10

$sw = [Diagnostics.Stopwatch]::StartNew()

$r = Invoke-RestMethod `
  -Uri "http://127.0.0.1:11434/api/chat" `
  -Method Post `
  -ContentType "application/json" `
  -Body $body

$sw.Stop()

Write-Host "Tiempo: $($sw.Elapsed.TotalSeconds) s"
$r | ConvertTo-Json -Depth 20
