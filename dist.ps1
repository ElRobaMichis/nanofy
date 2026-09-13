# Empaqueta Nanofy para repartirlo: compila en release y crea dist\Nanofy-<versión>-windows-x64.zip
# con solo el ejecutable y el LEEME. Uso:  .\dist.ps1   (o dist.cmd)
# Si defines $env:NANOFY_CLIENT_ID antes de compilar, ese Client ID queda como valor por defecto
# en Ajustes → Web API (tus amigos solo tendrán que pulsar "Conectar").
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;$env:PATH"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "La compilación falló" }

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$stage = Join-Path $root "dist\Nanofy"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "target\release\nanofy.exe" (Join-Path $stage "nanofy.exe")
Copy-Item "LEEME.txt" (Join-Path $stage "LEEME.txt")

$zip = Join-Path $root "dist\Nanofy-$version-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal
$mb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Output "Listo: $zip ($mb MB)"
