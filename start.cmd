@echo off
rem Launch the web UI from anywhere: double-click or run this file.
rem Works from any current directory and puts MinGW (needed by the pinned GNU toolchain) on PATH.
cd /d "%~dp0"
for /f "usebackq delims=" %%P in (`powershell -NoProfile -Command "[Environment]::GetEnvironmentVariable('Path','User') + ';' + [Environment]::GetEnvironmentVariable('Path','Machine')"`) do set "PATH=%%P"
cargo run --release --bin persona_web
pause
