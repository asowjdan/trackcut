@echo off
rem Double-click to open trackcut. Drop a video file on this file to open it.
rem Runs the development build: only changed code is rebuilt, so it starts quickly after an edit.
cd /d "%~dp0"
where cargo >nul 2>nul || (echo Rust is not installed. Install it from https://rustup.rs & pause & exit /b 1)
where ffmpeg >nul 2>nul || (echo FFmpeg is not installed or not on PATH. See https://ffmpeg.org & pause & exit /b 1)
cargo build -p trackcut-app || (pause & exit /b 1)
set "TARGET="
for /f "usebackq delims=" %%d in (`powershell -NoProfile -Command "(cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory"`) do set "TARGET=%%d"
if not exist "%TARGET%\debug\trackcut-app.exe" (echo trackcut-app.exe was not found. & pause & exit /b 1)
if "%TRACKCUT_NO_START%"=="1" (echo %TARGET%\debug\trackcut-app.exe & exit /b 0)
start "" "%TARGET%\debug\trackcut-app.exe" %*
