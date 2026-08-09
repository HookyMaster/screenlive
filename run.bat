@echo off
REM ScreenLive launcher - uses project venv, no console window
cd /d "%~dp0"
if not exist ".venv\Scripts\pythonw.exe" (
    echo venv not found. Run: python -m venv .venv
    pause
    exit /b 1
)
start "" ".venv\Scripts\pythonw.exe" main.py --gui
