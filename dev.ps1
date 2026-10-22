# dev.ps1 – Inicia o Rscover em modo desenvolvimento
# Garante que o cargo está no PATH independente do estado do terminal

$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH

Write-Host ">>> Cargo: $(cargo --version)" -ForegroundColor Green
Write-Host ">>> Iniciando Rscover dev server..." -ForegroundColor Cyan

npm run tauri dev
