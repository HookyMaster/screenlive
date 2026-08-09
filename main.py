"""ScreenLive - 鼠标自动追踪录屏工具入口(多比例, 像素任意)。

用法:
    pythonw main.py --gui                # 打开图形界面(无控制台窗口)
    python main.py                       # CLI 模式, 自动开始, Ctrl+C 停止
    python main.py --duration 30         # 录制 30 秒自动停止
    python main.py --size 16:9 --fps 24 -o out.mp4
    python main.py --size 1280x720       # 任意像素
"""

from __future__ import annotations

import argparse
import os
import time
from datetime import datetime

from screenlive.encoder import build_encoder_command, detect_hardware_encoder, find_ffmpeg
from screenlive.recorder import ScreenRecorder
from screenlive.tracker import SIZE_PRESETS, set_dpi_aware

#: 常用比例 -> 默认输出像素
_RATIO_DEFAULTS: dict[str, tuple[int, int]] = {
    "9:16": (810, 1440),
    "16:9": (1440, 810),
    "1:1": (1080, 1080),
    "3:4": (1080, 1440),
    "4:3": (1440, 1080),
    "9:21": (622, 1450),
}

_F9_PRESSED = False


def default_output() -> str:
    d = os.path.join(os.path.expanduser("~"), "Videos", "ScreenLive")
    os.makedirs(d, exist_ok=True)
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    return os.path.join(d, f"screenlive_{ts}.mp4")


def resolve_size(name: str) -> tuple[int, int]:
    """把 CLI 尺寸解析成 (宽, 高)。

    支持:
    - "大/中/小"(9:16 档位)
    - "9:16" "16:9" 等常用比例(用默认像素)
    - "宽x高" 任意像素(如 1280x720)
    - "宽:高@像素高"(如 16:9@720, 比例+指定高度)
    - 单个数字(视为高度, 按 9:16 推宽)
    """
    name = name.strip()
    # 比例@高度 形式: 16:9@720
    if "@" in name and ":" in name:
        try:
            ratio_part, h_part = name.split("@")
            rw, rh = (int(p) for p in ratio_part.split(":"))
            h = int(h_part)
            return _even(round(h * rw / rh)), _even(h)
        except (ValueError, TypeError):
            pass
    # 常用比例
    for r, (w, h) in _RATIO_DEFAULTS.items():
        if name == r:
            return w, h
    # 大/中/小 9:16 档位
    if name in SIZE_PRESETS:
        h = SIZE_PRESETS[name]
        return _even(round(h * 9 / 16)), _even(h)
    for k, v in SIZE_PRESETS.items():
        if k.startswith(name):
            h = v
            return _even(round(h * 9 / 16)), _even(h)
    # 宽x高 任意像素
    if "x" in name.lower():
        try:
            w, h = name.lower().split("x")
            return max(2, int(w) & ~1), max(2, int(h) & ~1)
        except ValueError:
            pass
    # 单个数字 -> 高度, 按 9:16 推宽
    try:
        h = int(name)
        return _even(round(h * 9 / 16)), _even(h)
    except ValueError:
        return 810, 1440


def _even(v: int) -> int:
    return max(2, int(v) & ~1)


def install_hotkey(toggle_cb) -> None:
    """F9 全局热键(可选依赖 pynput, 失败则忽略)。"""
    global _F9_PRESSED
    try:
        from pynput import keyboard
    except Exception:
        return

    def _on_press(key) -> None:
        global _F9_PRESSED
        try:
            if key == keyboard.Key.f9 and not _F9_PRESSED:
                _F9_PRESSED = True
                toggle_cb()
        except Exception:
            pass

    def _on_release(key) -> None:
        global _F9_PRESSED
        try:
            if key == keyboard.Key.f9:
                _F9_PRESSED = False
        except Exception:
            pass

    try:
        listener = keyboard.Listener(on_press=_on_press, on_release=_on_release)
        listener.daemon = True
        listener.start()
    except Exception:
        pass


def run_cli(args: argparse.Namespace) -> None:
    ffmpeg = find_ffmpeg()
    hw = detect_hardware_encoder(ffmpeg)
    enc = build_encoder_command(args.quality, hw)
    output = args.output or default_output()
    tw, th = resolve_size(args.size)

    print(f"ffmpeg   : {ffmpeg}")
    print(f"编码器   : {enc.name}")
    print(f"输出     : {output} ({tw}×{th})")

    recorder = ScreenRecorder(
        ffmpeg=ffmpeg,
        fps=args.fps,
        target_w=tw,
        target_h=th,
        smooth=args.smooth,
        show_cursor=not args.no_cursor,
        on_status=lambda st: print(
            f"\r  {st['elapsed']:6.1f}s | 实际 {st['fps']:5.1f} fps | "
            f"捕获 {st['captured']} | 编码 {st['encoded']} | 丢帧 {st['dropped']}    ",
            end="", flush=True,
        ),
    )
    recording = [False]

    def toggle() -> None:
        if recording[0]:
            recorder.stop_recording()
            recording[0] = False
        else:
            recorder.start_recording(output, enc)
            recording[0] = True

    # 先装热键再启动捕获: pynput 首次导入较慢(约 1~2s),
    # 若在捕获线程启动后导入会争抢 GIL, 导致首帧延迟、视频时长不足
    install_hotkey(toggle)

    recorder.start()  # 常驻捕获(CLI 下无预览, 仅保持管道一致)

    # CLI 模式直接开始录制(duration 到点自动停止; 无 duration 则 Ctrl+C/F9 停止)
    toggle()
    print("录制中… 按 F9 停止/继续, Ctrl+C 退出")

    try:
        while True:
            if recording[0] and args.duration and \
                    time.monotonic() - recorder.start_time >= args.duration:
                toggle()
                break
            if not recording[0] and args.duration:
                break  # F9 手动停止后退出
            time.sleep(0.1)
    except KeyboardInterrupt:
        pass

    if recorder.is_recording:
        recorder.stop_recording()
    recorder.shutdown()

    print("\n完成 ✔")
    print(f"  捕获 {recorder.captured} 帧 | 编码 {recorder.encoded} 帧 | 丢帧 {recorder.dropped}")
    if recorder.ffmpeg_error:
        print(f"  ⚠ {recorder.ffmpeg_error}")
    if os.path.exists(output):
        print(f"  文件: {output} ({os.path.getsize(output) / 1e6:.1f} MB)")
    else:
        print("  文件: 未生成")


def main() -> None:
    set_dpi_aware()
    parser = argparse.ArgumentParser(
        description="鼠标自动追踪录屏工具(支持多种比例)",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--gui", action="store_true", help="打开图形界面")
    parser.add_argument("-o", "--output", default="", help="输出 mp4 路径")
    parser.add_argument("--fps", type=int, default=30, help="帧率")
    parser.add_argument("--duration", type=float, default=0, help="录制秒数, 0 为手动停止")
    parser.add_argument("--size", default="中",
                        help="窗口尺寸: 大/中/小(9:16档位), 宽x高任意像素(如 1280x720), 或单个数字(高度,按9:16)")
    parser.add_argument("--smooth", type=float, default=0.85,
                        help="镜头跟随速度 (0.3~1.0, 1.0=鼠标始终居中)")
    parser.add_argument("--quality", choices=["high", "medium", "fast"],
                        default="high", help="画质/编码速度")
    parser.add_argument("--no-cursor", action="store_true", help="不渲染鼠标光标")
    args = parser.parse_args()

    if args.gui:
        from screenlive.gui import run_gui
        ffmpeg = find_ffmpeg()
        hw = detect_hardware_encoder(ffmpeg)
        # F9 全局热键 -> 经 Tk 主循环调度, 线程安全
        run_gui(ffmpeg, hw, on_ready=lambda gui: install_hotkey(
            lambda: gui.root.after(0, gui.toggle)))
        return

    run_cli(args)


if __name__ == "__main__":
    main()
