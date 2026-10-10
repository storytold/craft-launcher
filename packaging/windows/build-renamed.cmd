@echo off
rem Double-click to open the window that builds ArtCraft Launcher under another name (build-renamed.ps1).
where pwsh >nul 2>nul || (echo PowerShell 7 ^(pwsh^) is needed: https://aka.ms/powershell & pause & exit /b 1)
pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-renamed.ps1" %*
if errorlevel 1 pause
