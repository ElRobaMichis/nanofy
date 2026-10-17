@echo off
rem Crea el zip para repartir (dist\Nanofy-<versión>-windows-x64.zip). Ver dist.ps1.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0dist.ps1"
