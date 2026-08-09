@echo off
REM ScreenLive 启动脚本 - 使用 E 盘项目内 .venv, 无控制台窗口
cd /d "%~dp0"
if not exist ".venv\Scripts\pythonw.exe" (
    echo 未找到 .venv, 请先运行: python -m venv .venv
    pause
    exit /b 1
)
start "" ".venv\Scripts\pythonw.exe" main.py --gui
