@echo off
rem Compila Nanofy en modo release (toolchain GNU de Rust + gcc de WinLibs) y luego lo abre.
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;%PATH%"
cd /d "%~dp0"
cargo build --release %*
if errorlevel 1 (
  echo.
  echo La compilacion ha fallado. Si Nanofy esta abierto, cierralo y vuelve a ejecutar build.cmd.
  pause
  exit /b 1
)
start "" "%~dp0target\release\nanofy.exe"
endlocal
