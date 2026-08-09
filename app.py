"""ScreenLive 打包入口(PyInstaller)。

双击 exe 直接进入图形界面(GUI 模式), 跳过 argparse 命令行解析。
与 main.py --gui 行为一致: 设置窗口 + 实时预览 + F9 全局热键。
"""

from __future__ import annotations

from main import install_hotkey
from screenlive.encoder import detect_hardware_encoder, find_ffmpeg
from screenlive.gui import run_gui
from screenlive.tracker import set_dpi_aware


def main() -> None:
    set_dpi_aware()
    ffmpeg = find_ffmpeg()
    hw = detect_hardware_encoder(ffmpeg)
    # F9 全局热键 -> 经 Tk 主循环调度, 线程安全
    run_gui(ffmpeg, hw, on_ready=lambda gui: install_hotkey(
        lambda: gui.root.after(0, gui.toggle)))


if __name__ == "__main__":
    main()
