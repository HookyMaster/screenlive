"""Tkinter 界面: 设置窗口 + 独立实时预览窗口。

布局:
- 设置窗口: 比例/像素/帧率/跟随/光标/排除本窗口/保存目录/开始录制。
- 预览窗口: 独立 Toplevel, 实时显示框选画面; 拖动窗口边缘可等比例缩放,
  缩放只影响预览大小, 不影响输出像素。

比例与像素:
- 常用比例预设(9:16 / 16:9 / 1:1 / 3:4 / 4:3 / 9:21): 宽高输入框联动,
  改高自动推宽、改宽自动推高。
- 选"自定义"后宽高完全自由, 任意像素。
"""

from __future__ import annotations

import ctypes
import json
import os
import sys
import threading
import tkinter as tk
from datetime import datetime
from tkinter import filedialog, ttk

import numpy as np
from PIL import Image, ImageTk

from .recorder import ScreenRecorder
from .tracker import RATIO_PRESETS

__all__ = ["run_gui"]

# 预览窗口: 无边框窗口(overrideredirect), 拖拽移动/等比缩放全部由
# Tk 鼠标事件自实现 —— 没有标题栏/边框, 就没有系统最小跟踪尺寸限制
# (高 DPI 下带标题栏窗口最小约 328px 宽, 做不出屏幕 1/8 的小预览)。
# 等比实时精确、零抖动, 也不再有 Win32 回调崩溃的风险。
_GRIP = 14  # 边缘/角落抓取区宽度(物理像素)
_CURSOR_MAP = {
    "n": "sb_v_double_arrow", "s": "sb_v_double_arrow",
    "e": "sb_h_double_arrow", "w": "sb_h_double_arrow",
    "ne": "size_ne_sw", "sw": "size_ne_sw",
    "nw": "size_nw_se", "se": "size_nw_se",
    "move": "fleur",
}

# 配置文件: 开发时放项目根目录; PyInstaller 打包后放到用户目录
# (打包目录只读/可能被移动, 不能写配置)。
if getattr(sys, "frozen", False):
    _cfg_dir = os.path.join(os.path.expanduser("~"), ".screenlive")
    try:
        os.makedirs(_cfg_dir, exist_ok=True)
    except Exception:
        _cfg_dir = os.path.expanduser("~")
    CONFIG_PATH = os.path.join(_cfg_dir, "config.json")
else:
    CONFIG_PATH = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "config.json"
    )
DEFAULT_OUTDIR = os.path.join(os.path.expanduser("~"), "Videos", "ScreenLive")

#: 各比例预设的默认输出像素
_RATIO_DEFAULTS: dict[str, tuple[int, int]] = {
    "9:16 (竖屏手机)": (810, 1440),
    "16:9 (横屏)": (1440, 810),
    "1:1 (方形)": (1080, 1080),
    "3:4": (1080, 1440),
    "4:3": (1440, 1080),
    "9:21 (长屏)": (622, 1450),
}


def _load_config() -> dict:
    try:
        with open(CONFIG_PATH, encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}


def _save_config(cfg: dict) -> None:
    try:
        with open(CONFIG_PATH, "w", encoding="utf-8") as f:
            json.dump(cfg, f, ensure_ascii=False, indent=2)
    except Exception:
        pass


def _find_icon_path() -> str:
    """定位窗口图标: 打包后从 _MEIPASS(经 --add-data 打入), 开发用项目根。
    优先 .ico(iconbitmap 用, 窗口映射后设置才生效)。"""
    names = ("icon.ico", "icon256.png")
    if getattr(sys, "frozen", False):
        for base in (getattr(sys, "_MEIPASS", ""),
                     os.path.dirname(sys.executable)):
            if not base:
                continue
            for n in names:
                cand = os.path.join(base, n)
                if os.path.exists(cand):
                    return cand
        return ""
    root_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    for n in names:
        cand = os.path.join(root_dir, n)
        if os.path.exists(cand):
            return cand
    return ""


def set_capture_exclusion(win, enable: bool) -> bool:
    """设置窗口捕获排除(WDA_EXCLUDEFROMCAPTURE): 勾上后抓屏/录屏 API
    会跳过该窗口, 只录到窗口之下的内容。需要 Win10 2004+ (build 19041+)。

    win 可以是 Tk 主窗口或 Toplevel。返回是否设置成功。
    """
    try:
        import ctypes
        import sys
        if sys.getwindowsversion().build < 19041:
            return False
        # winfo_id 返回客户区句柄, GetAncestor(GA_ROOT=2) 取真正的顶层窗口
        hwnd = ctypes.windll.user32.GetAncestor(int(win.winfo_id()), 2)
        if not hwnd:
            return False
        # WDA_NONE=0, WDA_EXCLUDEFROMCAPTURE=17(0x11)
        affinity = 17 if enable else 0
        ok = ctypes.windll.user32.SetWindowDisplayAffinity(hwnd, affinity)
        return bool(ok)
    except Exception:
        return False


def _even(v) -> int:
    """转偶数像素(h264/yuv420p 要求)。"""
    return max(2, int(v) & ~1)


class RecorderGUI:
    def __init__(self, root: tk.Tk, ffmpeg: str, hw_encoder: str | None) -> None:
        self.root = root
        self.ffmpeg = ffmpeg
        self.hw_encoder = hw_encoder
        self.recorder: ScreenRecorder | None = None
        self._tk_img: ImageTk.PhotoImage | None = None  # 保持引用防 GC
        self._latest_frame: np.ndarray | None = None
        self._latest_region: dict | None = None
        self._latest_status: dict | None = None  # 捕获线程写, Tk 定时器读
        self._frame_lock = threading.Lock()

        # 预览窗口拖拽状态(无边框窗口, Tk 鼠标事件自实现移动/等比缩放)
        self._drag: dict | None = None
        self._pv_cursor = ""  # 当前光标(避免重复设置)
        self._pv_label = "实时预览"
        self._screen = (0, 0)

        # 像素输入防抖状态(见 _apply_pixel/_link_pixel)
        self._pixel_master = "height"  # 最后编辑的维度
        self._pixel_after_id: str | None = None
        self._pixel_updating = False  # 联动 set 时置位, 防 trace 级联

        # 音频设备枚举状态(后台线程写 pending, 主循环消费)
        self._audio_loading = False
        self._audio_devs_pending: list[str] | None = None

        cfg = _load_config()

        root.title("ScreenLive · 录制设置")

        # 窗口图标(标题栏/任务栏): iconbitmap 必须等窗口映射后才生效,
        # 故延迟到 after() 再设置(开发用项目根 icon.ico, 打包后从 _MEIPASS 取)
        self._icon_path = _find_icon_path()
        if self._icon_path:
            root.after(80, self._apply_window_icon)

        self._build_ui(cfg)

        # 设置窗口: 按内容自适应, 屏幕居中偏上
        import ctypes
        sw = ctypes.windll.user32.GetSystemMetrics(0)
        sh = ctypes.windll.user32.GetSystemMetrics(1)
        root.update_idletasks()
        win_w = max(root.winfo_reqwidth(), int(sw * 0.24))
        win_h = max(root.winfo_reqheight(), 300)
        x = max(0, sw - win_w - int(sw * 0.03))
        y = max(0, (sh - win_h) // 5)
        root.geometry(f"+{x}+{y}")
        root.resizable(False, False)

        from .encoder import build_encoder_command
        enc_name = build_encoder_command("high", hw_encoder).name
        self.status.set(
            f"编码器: {enc_name}  |  F9 开始/停止录制"
        )

        # ---- 预览窗口 ----
        self._ratio_w = 9
        self._ratio_h = 16
        self._apply_ratio_to_internal()
        self._build_preview_window(cfg)

        # 常驻捕获线程: 立即开始实时预览
        self._new_recorder()
        self.root.after(100, self._poll_preview)

        # 应用"不录制本窗口"(设置窗口 + 预览窗口都排除)
        self.root.after(250, self._apply_exclusion)

    # ------------------------------------------------------------------ #
    # 比例/像素
    # ------------------------------------------------------------------ #
    def _apply_ratio_to_internal(self) -> None:
        ratio = self.ratio_var.get()
        rw, rh = RATIO_PRESETS.get(ratio, (0, 0))
        if rw and rh:
            self._ratio_w, self._ratio_h = rw, rh

    def _current_px(self) -> tuple[int, int]:
        try:
            w = int(self.width_var.get())
            h = int(self.height_var.get())
        except (ValueError, tk.TclError):
            return 810, 1440
        return max(2, w), max(2, h)

    def _on_ratio_change(self, *_a) -> None:
        """切换比例预设: 按默认像素联动宽高; 自定义则不动。"""
        self._apply_ratio_to_internal()
        ratio = self.ratio_var.get()
        if ratio in _RATIO_DEFAULTS:
            w, h = _RATIO_DEFAULTS[ratio]
            self.width_var.set(w)
            self.height_var.set(h)
            self._sync_preview_ratio()
            self._apply_output_size()
        self._save_cfg()

    def _on_width_edit(self, *_a) -> None:
        """改宽: 记录主维度并防抖(停止输入 300ms 后联动/应用)。"""
        if self._pixel_updating:
            return
        self._pixel_master = "width"
        self._pixel_debounce()

    def _on_height_edit(self, *_a) -> None:
        if self._pixel_updating:
            return
        self._pixel_master = "height"
        self._pixel_debounce()

    def _pixel_debounce(self) -> None:
        """防抖: 取消上一次的延迟应用, 停止输入 300ms 后再执行。"""
        if self._pixel_after_id:
            try:
                self.root.after_cancel(self._pixel_after_id)
            except Exception:
                pass
        self._pixel_after_id = self.root.after(300, self._apply_pixel)

    def _apply_pixel(self) -> None:
        """输入停顿后: 预设比例联动 -> 预览比例 -> 应用到捕获。"""
        self._pixel_after_id = None
        if self._pixel_updating:
            return
        self._pixel_updating = True
        try:
            self._link_pixel(self._pixel_master)
        finally:
            self._pixel_updating = False
        self._sync_preview_ratio()
        self._apply_output_size()
        self._save_cfg()

    def _link_pixel(self, master: str) -> None:
        """预设比例下联动宽高; 自定义不联动。

        注意: 联动只改写输入框显示(保留用户输入, 不强制偶数),
        录制时由 _apply_output_size 内部做偶数化, 所见即所输。
        """
        ratio = self.ratio_var.get()
        rw, rh = RATIO_PRESETS.get(ratio, (0, 0))
        if not (rw and rh):
            return  # 自定义: 不联动
        try:
            if master == "width":
                w = int(self.width_var.get())
                if 2 <= w <= 8192:
                    self.height_var.set(str(round(w * rh / rw)))
            else:
                h = int(self.height_var.get())
                if 2 <= h <= 8192:
                    self.width_var.set(str(round(h * rw / rh)))
        except (ValueError, tk.TclError):
            pass

    def _sync_preview_ratio(self) -> None:
        """预览窗口比例跟随输出比例; 以当前预览宽度重算高度。"""
        w, h = self._current_px()
        self._ratio_w, self._ratio_h = max(1, w), max(1, h)
        self._update_preview_title()
        # 若预览窗口已存在, 按新比例调整尺寸(保持当前宽度, 重算高度)
        if getattr(self, "preview_win", None) and self.preview_win.winfo_exists():
            try:
                cw = self.preview_win.winfo_width()
                if cw > 50:
                    nh = max(100, int(round(cw * h / w)))
                    x = self.preview_win.winfo_x()
                    y = self.preview_win.winfo_y()
                    self.preview_win.geometry(f"{cw}x{nh}+{x}+{y}")
            except Exception:
                pass

    def _apply_output_size(self) -> None:
        """把宽高输入应用到常驻捕获(录制中禁止; 内部偶数化, 不改输入框)。"""
        if not self.recorder:
            return
        try:
            w = int(self.width_var.get())
            h = int(self.height_var.get())
        except (ValueError, tk.TclError):
            return  # 输入非法/未完成: 不应用
        if not (2 <= w <= 8192 and 2 <= h <= 8192):
            return
        if self.recorder.is_recording:
            self.status.set("⚠ 录制中不能改尺寸, 请先停止")
            return
        self.recorder.set_size(_even(w), _even(h))
        self._update_preview_title()

    # ------------------------------------------------------------------ #
    # 设置窗口 UI
    # ------------------------------------------------------------------ #
    def _build_ui(self, cfg: dict) -> None:
        frm = ttk.Frame(self.root, padding=14)
        frm.pack(fill="both", expand=True)
        pad = {"padx": 8, "pady": 7}

        row = 0
        ttk.Label(frm, text="ScreenLive · 录制设置",
                  font=("Microsoft YaHei UI", 12, "bold")).grid(
            row=row, column=0, columnspan=3, sticky="w", **pad)

        # 比例预设
        row += 1
        ttk.Label(frm, text="画面比例:").grid(row=row, column=0, sticky="w", **pad)
        self.ratio_var = tk.StringVar(
            value=cfg.get("ratio", "9:16 (竖屏手机)"))
        if self.ratio_var.get() not in RATIO_PRESETS:
            self.ratio_var.set("9:16 (竖屏手机)")
        ratio_cb = ttk.Combobox(
            frm, textvariable=self.ratio_var, width=17, state="readonly",
            values=list(RATIO_PRESETS.keys()),
        )
        ratio_cb.grid(row=row, column=1, columnspan=2, sticky="w", **pad)

        # 像素(任意)
        row += 1
        ttk.Label(frm, text="输出像素:").grid(row=row, column=0, sticky="w", **pad)
        self.width_var = tk.StringVar(value=str(cfg.get("width", 810)))
        self.height_var = tk.StringVar(value=str(cfg.get("height", 1440)))
        wf = ttk.Frame(frm)
        wf.grid(row=row, column=1, columnspan=2, sticky="w")
        ttk.Entry(wf, textvariable=self.width_var, width=7).pack(side="left")
        ttk.Label(wf, text=" × ").pack(side="left")
        ttk.Entry(wf, textvariable=self.height_var, width=7).pack(side="left")
        ttk.Label(wf, text=" px").pack(side="left")

        # 帧率
        row += 1
        ttk.Label(frm, text="帧率 (fps):").grid(row=row, column=0, sticky="w", **pad)
        self.fps_var = tk.IntVar(value=int(cfg.get("fps", 30)))
        ttk.Combobox(
            frm, textvariable=self.fps_var, width=6, state="readonly",
            values=[15, 24, 30, 45, 60],
        ).grid(row=row, column=1, columnspan=2, sticky="w", **pad)

        # 跟随速度
        row += 1
        ttk.Label(frm, text="跟随速度:").grid(row=row, column=0, sticky="w", **pad)
        self.smooth_var = tk.DoubleVar(value=float(cfg.get("smooth", 0.85)))
        ttk.Scale(frm, from_=0.3, to=1.0, variable=self.smooth_var,
                  command=lambda _v: None).grid(
            row=row, column=1, columnspan=2, sticky="ew", **pad)

        # 光标
        row += 1
        self.cursor_var = tk.BooleanVar(value=bool(cfg.get("cursor", True)))
        ttk.Checkbutton(frm, text="视频中显示鼠标光标",
                        variable=self.cursor_var).grid(
            row=row, column=0, columnspan=3, sticky="w", **pad)

        # 录音(默认开启: 声音直接集成进视频, 单文件双流)
        row += 1
        self.audio_var = tk.BooleanVar(value=bool(cfg.get("audio", True)))
        ttk.Checkbutton(frm, text="录制声音",
                        variable=self.audio_var,
                        command=self._on_audio_toggle).grid(
            row=row, column=0, sticky="w", **pad)
        self.audio_dev_var = tk.StringVar(value=str(cfg.get("audio_dev", "")))
        self.audio_dev_cb = ttk.Combobox(
            frm, textvariable=self.audio_dev_var, width=28,
            state="disabled", values=[],
        )
        self.audio_dev_cb.grid(row=row, column=1, columnspan=2, sticky="w", **pad)
        self._on_audio_toggle()  # 按 config 初始化下拉可用状态

        # 排除本窗口
        row += 1
        self.exclude_var = tk.BooleanVar(value=bool(cfg.get("exclude_self", True)))
        ttk.Checkbutton(
            frm, text="不录制本软件窗口",
            variable=self.exclude_var, command=self._apply_exclusion,
        ).grid(row=row, column=0, columnspan=3, sticky="w", **pad)

        # 保存目录
        row += 1
        ttk.Label(frm, text="保存目录:").grid(row=row, column=0, sticky="nw", **pad)
        self.outdir_var = tk.StringVar(
            value=cfg.get("outdir", DEFAULT_OUTDIR))
        ttk.Entry(frm, textvariable=self.outdir_var, width=26).grid(
            row=row, column=1, sticky="ew", **pad)
        ttk.Button(frm, text="选择…", width=6,
                   command=self._choose_dir).grid(row=row, column=2, sticky="w")

        # 开始/停止按钮
        row += 1
        self.btn = ttk.Button(frm, text="▶  开始录制  (F9)", command=self.toggle)
        self.btn.grid(row=row, column=0, columnspan=3, sticky="ew",
                      pady=12, padx=8)

        # 预览窗口控制
        row += 1
        pf = ttk.Frame(frm)
        pf.grid(row=row, column=0, columnspan=3, sticky="ew")
        ttk.Button(pf, text="显示/隐藏预览",
                   command=self._toggle_preview).pack(side="left", padx=(8, 4))
        ttk.Label(pf, text="预览窗口可拖边缘等比缩放",
                  foreground="#888").pack(side="left", padx=4)

        # 状态区
        row += 1
        self.status = tk.StringVar(value="正在启动…")
        ttk.Label(frm, textvariable=self.status, foreground="#555",
                  wraplength=340, justify="left").grid(
            row=row, column=0, columnspan=3, sticky="w", **pad)

        frm.columnconfigure(1, weight=1)

        # 事件绑定
        ratio_cb.bind("<<ComboboxSelected>>", self._on_ratio_change)
        self.width_var.trace_add("write", self._on_width_edit)
        self.height_var.trace_add("write", self._on_height_edit)

        self.root.protocol("WM_DELETE_WINDOW", self._on_close)

    # ------------------------------------------------------------------ #
    # 预览窗口(独立 Toplevel, 等比例缩放)
    # ------------------------------------------------------------------ #
    def _build_preview_window(self, cfg: dict) -> None:
        pw = tk.Toplevel(self.root)
        pw.title("实时预览")
        self.preview_win = pw

        # 无边框窗口: 没有标题栏/边框 => 没有系统最小跟踪尺寸,
        # 尺寸完全由自己控制, 默认可以做到屏幕的 1/8 高。
        pw.overrideredirect(True)

        self.canvas = tk.Canvas(pw, bg="black", highlightthickness=0)
        self.canvas.pack(fill="both", expand=True)
        # 关键: 禁止 Canvas 的请求尺寸反向撑大窗口
        pw.pack_propagate(False)

        import ctypes
        sw = ctypes.windll.user32.GetSystemMetrics(0)
        sh = ctypes.windll.user32.GetSystemMetrics(1)
        self._screen = (sw, sh)

        out_w, out_h = self._current_px()
        rw, rh = max(1, out_w), max(1, out_h)
        # 默认尺寸: 窗口高度 = 屏幕高度的 1/8, 宽度按输出比例推出
        default_client_h = max(120, int(sh / 8))
        default_client_w = max(120, int(round(default_client_h * rw / rh)))
        default_x = max(0, (sw - default_client_w) // 2)
        default_y = max(0, (sh - default_client_h) // 3)

        client_w = int(cfg.get("preview_w", default_client_w))
        if not (120 <= client_w <= sw - 200):
            client_w = default_client_w
        client_h = int(round(client_w * rh / rw))
        client_h = max(100, min(client_h, sh - 200))

        x = int(cfg.get("preview_x", default_x))
        y = int(cfg.get("preview_y", default_y))
        # 位置校验: 必须至少有一部分留在屏幕内(防止残留的越界配置)
        if x > sw - 80 or x < -client_w + 80 or y > sh - 40 or y < -client_h + 40:
            x, y = default_x, default_y

        pw.geometry(f"{client_w}x{client_h}+{x}+{y}")
        self._drag = None
        self._pv_cursor = ""

        # 鼠标: 拖身体移动窗口, 拖边缘/角落等比缩放(边缘有抓取区)
        self.canvas.bind("<ButtonPress-1>", self._pv_press)
        self.canvas.bind("<B1-Motion>", self._pv_drag)
        self.canvas.bind("<ButtonRelease-1>", self._pv_release)
        self.canvas.bind("<Motion>", self._pv_hover)
        self.canvas.bind("<Leave>", self._pv_leave)

        self._update_preview_title()

    def _update_preview_title(self) -> None:
        try:
            w, h = self._current_px()
            # 约分显示比例(900x1600 -> 9:16)
            g = _gcd(w, h)
            rw, rh = w // g, h // g
            self._pv_label = f"{w}×{h}  {rw}:{rh} 等比"
            self.preview_win.title(f"实时预览 · {w}×{h}")
        except Exception:
            pass

    # ------------------------------------------------------------------ #
    # 无边框窗口交互: 拖身体移动, 拖边缘/角落等比缩放(自定义实现)。
    # 等比以"输出像素"为准(_current_px), 宽高始终按比例联动, 无抖动。
    # ------------------------------------------------------------------ #
    def _pv_zone(self, x: int, y: int) -> str:
        """根据鼠标在画布内的位置判断抓取区域(移动/八方向缩放)。"""
        w = self.canvas.winfo_width()
        h = self.canvas.winfo_height()
        g = _GRIP
        left, right = x < g, x >= w - g
        top, bottom = y < g, y >= h - g
        if top and left:
            return "nw"
        if top and right:
            return "ne"
        if bottom and left:
            return "sw"
        if bottom and right:
            return "se"
        if left:
            return "w"
        if right:
            return "e"
        if top:
            return "n"
        if bottom:
            return "s"
        return "move"

    def _pv_hover(self, e) -> None:
        if self._drag:
            return
        cur = _CURSOR_MAP.get(self._pv_zone(e.x, e.y), "arrow")
        if cur != self._pv_cursor:
            self._pv_cursor = cur
            self.canvas.config(cursor=cur)

    def _pv_leave(self, _e) -> None:
        if not self._drag and self._pv_cursor != "arrow":
            self._pv_cursor = "arrow"
            self.canvas.config(cursor="arrow")

    def _pv_press(self, e) -> None:
        """按下: 记录起始状态(窗口位置/尺寸 + 鼠标锚点)。"""
        self._drag = {
            "mode": self._pv_zone(e.x, e.y),
            "mx0": e.x_root, "my0": e.y_root,
            "wx": self.preview_win.winfo_x(),
            "wy": self.preview_win.winfo_y(),
            "ww": self.preview_win.winfo_width(),
            "wh": self.preview_win.winfo_height(),
        }
        self.canvas.config(cursor=_CURSOR_MAP.get(self._drag["mode"], "fleur"))

    def _pv_drag(self, e) -> None:
        """拖动: 移动窗口, 或按输出比例实时等比缩放(锚定固定边)。"""
        d = self._drag
        if not d:
            return
        mode = d["mode"]
        dx = e.x_root - d["mx0"]
        dy = e.y_root - d["my0"]

        if mode == "move":
            sw, sh = self._screen
            x = d["wx"] + dx
            y = d["wy"] + dy
            # 轻度限制: 至少留一部分在屏幕内, 防止拖丢
            x = max(-d["ww"] + 80, min(x, sw - 80))
            y = max(-d["wh"] + 40, min(y, sh - 40))
            self.preview_win.geometry(f"+{int(x)}+{int(y)}")
            return

        # ---- 等比缩放 ----
        ow, oh = self._current_px()
        rw, rh = max(1, ow), max(1, oh)
        sw, sh = self._screen
        max_w, max_h = sw - 80, sh - 80
        # 比例感知的最小尺寸: 两维都 >= 各自下限, 且宽高比始终成立
        if rw >= rh:
            min_w = max(120, int(round(100 * rw / rh)))
            min_h = 100
        else:
            min_w = 120
            min_h = max(100, int(round(120 * rh / rw)))
        ww, wh = d["ww"], d["wh"]

        # 主维度: 拖左右边以宽为准, 拖上下边以高为准,
        # 拖角以相对变化量大的维度为准
        if mode in ("e", "w"):
            master = "w"
        elif mode in ("n", "s"):
            master = "h"
        else:
            master = "w" if abs(dx) / max(1, ww) >= abs(dy) / max(1, wh) else "h"

        if master == "w":
            nw_ = ww + (dx if mode in ("e", "ne", "se") else -dx)
            nw_ = max(min_w, min(nw_, max_w))
            nh_ = max(min_h, min(int(round(nw_ * rh / rw)), max_h))
            if nh_ <= min_h or nh_ >= max_h:  # 次维到界: 反推主维保持比例
                nw_ = max(min_w, min(int(round(nh_ * rw / rh)), max_w))
        else:
            nh_ = wh + (dy if mode in ("s", "se", "sw") else -dy)
            nh_ = max(min_h, min(nh_, max_h))
            nw_ = max(min_w, min(int(round(nh_ * rw / rh)), max_w))
            if nw_ <= min_w or nw_ >= max_w:  # 次维到界: 反推主维保持比例
                nh_ = max(min_h, min(int(round(nw_ * rh / rw)), max_h))

        # 锚定固定边: 拖右/下边固定左上角; 拖左/上边固定右下角
        x, y = d["wx"], d["wy"]
        if mode in ("w", "nw", "sw"):  # 右边固定, 左边跟随鼠标
            x = d["wx"] + ww - nw_
        if mode in ("n", "ne", "nw"):  # 底边固定, 顶边跟随鼠标
            y = d["wy"] + wh - nh_
        self.preview_win.geometry(f"{nw_}x{nh_}+{int(x)}+{int(y)}")

    def _pv_release(self, _e) -> None:
        """松开: 结束拖拽, 等 geometry 生效后保存位置/尺寸。"""
        if self._drag:
            self._drag = None
            self.preview_win.after(50, self._save_preview_geom)

    def _toggle_preview(self) -> None:
        """显示/隐藏预览窗口。"""
        if not self.preview_win.winfo_exists():
            self._build_preview_window(_load_config())
            self._apply_exclusion()
            return
        if self.preview_win.winfo_viewable():
            # 记住位置再隐藏
            self._save_preview_geom()
            self.preview_win.withdraw()
        else:
            self.preview_win.deiconify()
            # 隐藏->再显示后, WDA_EXCLUDEFROMCAPTURE 的 DWM 排除面会失效:
            # 亲和力标志仍为 17, 但捕获里该窗口区域变成黑块(预览与捕获区
            # 重叠时呈"黑框")。等窗口映射后重新 0->17 重建排除面
            # (等效手动"取消再勾选", 见 _refresh_exclusion)。
            self.preview_win.after(150, self._refresh_exclusion)
            self.preview_win.after(350, self._refresh_exclusion)

    # ------------------------------------------------------------------ #
    # 常驻捕获 / 预览渲染
    # ------------------------------------------------------------------ #
    def _new_recorder(self) -> None:
        if self.recorder:
            self.recorder.shutdown()
        w, h = self._current_px()
        self.recorder = ScreenRecorder(
            ffmpeg=self.ffmpeg,
            fps=int(self.fps_var.get()),
            target_w=_even(w),
            target_h=_even(h),
            smooth=float(self.smooth_var.get()),
            show_cursor=bool(self.cursor_var.get()),
            on_frame=self._on_capture_frame,
            on_status=self._on_status,
        )
        self.recorder.start()

    def _on_capture_frame(self, frame: np.ndarray, region: dict,
                          mouse_rel: tuple) -> None:
        with self._frame_lock:
            self._latest_frame = frame
            self._latest_region = region

    def _poll_preview(self) -> None:
        """每 40ms 取最新一帧渲染到预览画布(按画布当前尺寸缩放)。"""
        with self._frame_lock:
            frame = self._latest_frame
            self._latest_frame = None
            devs = getattr(self, "_audio_devs_pending", None)
            if devs is not None:
                self._audio_devs_pending = None

        # 音频设备枚举结果(后台线程写, 主循环消费)
        if devs is not None:
            self._audio_devices_ready(devs)

        if frame is not None and self.preview_win.winfo_exists() \
                and self.preview_win.winfo_viewable():
            try:
                cw = self.canvas.winfo_width()
                ch = self.canvas.winfo_height()
                if cw > 50 and ch > 50:
                    fh, fw = frame.shape[:2]
                    step_y = max(1, fh // ch)
                    step_x = max(1, fw // cw)
                    small = frame[::step_y, ::step_x, :3][:, :, ::-1]  # BGRA->RGB
                    img = Image.fromarray(np.ascontiguousarray(small))
                    img = img.resize((cw, ch), Image.BILINEAR)
                    self._tk_img = ImageTk.PhotoImage(img)
                    self.canvas.delete("img")
                    self.canvas.create_image(0, 0, image=self._tk_img,
                                             anchor="nw", tags="img")
                    # 信息条压到图像之上(左上角显示输出尺寸/比例)
                    self.canvas.tag_lower("img")
                    self.canvas.delete("ovl")
                    ti = self.canvas.create_text(
                        6, 5, text=self._pv_label, anchor="nw",
                        font=("Microsoft YaHei UI", 8), fill="#ffffff",
                        tags="ovl")
                    x0, y0, x1, y1 = self.canvas.bbox(ti)
                    self.canvas.create_rectangle(
                        x0 - 3, y0 - 2, x1 + 3, y1 + 2,
                        fill="#333333", stipple="gray50", outline="",
                        tags="ovl")
            except Exception:
                pass

        # 录制中状态叠加(读取捕获线程写入的最新状态)
        if self.recorder and self.recorder.is_recording:
            with self._frame_lock:
                st = self._latest_status
            if st:
                self.status.set(
                    f"录制中 {st['elapsed']:.1f}s | 实际 {st['fps']:.1f} fps | "
                    f"丢帧 {st['dropped']}"
                )
            r = self.recorder
            elapsed = _elapsed(r.start_time)
            self.root.title(
                f"ScreenLive · 录制中 {elapsed:.0f}s | 编码 {r.encoded} | 丢帧 {r.dropped}")
        else:
            self.root.title("ScreenLive · 录制设置")

        self.root.after(40, self._poll_preview)

    # ------------------------------------------------------------------ #
    def _on_audio_toggle(self, *_a) -> None:
        """勾选录音: 启用设备下拉(若设备列表为空则触发枚举)。"""
        if self.audio_var.get():
            self.audio_dev_cb.configure(state="readonly")
            if not self.audio_dev_cb["values"] and self.ffmpeg:
                self._load_audio_devices()
        else:
            self.audio_dev_cb.configure(state="disabled")

    def _load_audio_devices(self) -> None:
        """后台枚举音频输入设备(ffmpeg dshow)。

        注意: 后台线程不能调用任何 Tk API(root.after 会报
        "main thread is not in main loop"), 只写 pending 变量,
        由 _poll_preview(Tk 主循环) 消费。
        """
        if getattr(self, "_audio_loading", False):
            return
        self._audio_loading = True

        def work() -> None:
            try:
                from .encoder import list_audio_devices
                devs = list_audio_devices(self.ffmpeg)
            except Exception:
                devs = []
            with self._frame_lock:
                self._audio_devs_pending = devs

        threading.Thread(target=work, daemon=True).start()

    def _audio_devices_ready(self, devs: list[str]) -> None:
        self._audio_loading = False
        self.audio_dev_cb["values"] = devs
        cur = self.audio_dev_var.get()
        if cur not in devs and devs:
            self.audio_dev_var.set(devs[0])
        if self.audio_var.get():
            self.audio_dev_cb.configure(state="readonly")
            if not devs:
                self.status.set("⚠ 未找到音频输入设备(麦克风/立体声混音)")

    def _choose_dir(self) -> None:
        d = filedialog.askdirectory(
            title="选择视频保存目录",
            initialdir=self.outdir_var.get() or DEFAULT_OUTDIR,
            mustexist=True,
        )
        if d:
            self.outdir_var.set(d)
            self._save_cfg()

    def _save_cfg(self) -> None:
        w, h = self._current_px()
        cfg = _load_config()  # 合并, 避免清掉其他键
        cfg.update({
            "ratio": self.ratio_var.get(),
            "width": w,
            "height": h,
            "outdir": self.outdir_var.get().strip(),
            "fps": int(self.fps_var.get()),
            "smooth": float(self.smooth_var.get()),
            "cursor": bool(self.cursor_var.get()),
            "audio": bool(self.audio_var.get()),
            "audio_dev": self.audio_dev_var.get().strip(),
            "exclude_self": bool(self.exclude_var.get()),
        })
        _save_config(cfg)

    def _save_preview_geom(self) -> None:
        """保存预览窗口位置/尺寸(无边框窗口: winfo 即客户区真实值)。

        只在尺寸稳定时调用(拖拽结束/隐藏/关闭), 避免写入中间值。
        """
        try:
            if not self.preview_win.winfo_exists():
                return
            cfg = _load_config()
            cfg["preview_x"] = self.preview_win.winfo_x()
            cfg["preview_y"] = self.preview_win.winfo_y()
            cfg["preview_w"] = self.preview_win.winfo_width()
            _save_config(cfg)
        except Exception:
            pass

    # ------------------------------------------------------------------ #
    # 录制启停
    # ------------------------------------------------------------------ #
    def toggle(self) -> None:
        if self.recorder and self.recorder.is_recording:
            self._stop()
        else:
            self._start()

    def _start(self) -> None:
        outdir = self.outdir_var.get().strip()
        if not outdir or not os.path.isdir(outdir):
            outdir = filedialog.askdirectory(title="选择视频保存目录")
            if not outdir:
                return
            self.outdir_var.set(outdir)
        os.makedirs(outdir, exist_ok=True)

        ts = datetime.now().strftime("%Y%m%d_%H%M%S")
        output = os.path.join(outdir, f"screenlive_{ts}.mp4")

        from .encoder import build_encoder_command
        enc = build_encoder_command("medium", self.hw_encoder)

        self._save_cfg()
        # 录制前确保排除面新鲜(隐藏/显示预览可能使 DWM 排除面失效)
        self._refresh_exclusion()
        # 参数若改动过, 先重建捕获
        w, h = self._current_px()
        if self.recorder and (
                self.recorder.fps != int(self.fps_var.get())
                or self.recorder.show_cursor != bool(self.cursor_var.get())
                or self.recorder.smooth != float(self.smooth_var.get())
                or (self.recorder.target_w, self.recorder.target_h) != (_even(w), _even(h))):
            self._new_recorder()

        # 录音: 声音直接集成进视频(单文件含视频+音频流); 勾选但设备为空时
        # 明确警告, 避免静默录出无声视频
        audio_dev = None
        if self.audio_var.get():
            audio_dev = self.audio_dev_var.get().strip()
            if not audio_dev:
                self.status.set("⚠ 已勾选录制声音但未选择音频设备, 本次无音轨")
        self.recorder.start_recording(output, enc, audio_device=audio_dev)
        self.btn.configure(text="■  停止录制  (F9)")
        w2, h2 = self.recorder.target_w, self.recorder.target_h
        self.status.set(f"录制中 {w2}×{h2} → {os.path.basename(output)}")

    def _stop(self) -> None:
        if not self.recorder:
            return
        r = self.recorder
        r.stop_recording()
        self.btn.configure(text="▶  开始录制  (F9)")
        msg = (
            f"已保存: {os.path.basename(r.output_path)}\n"
            f"捕获 {r.captured} 帧 | 编码 {r.encoded} 帧 | 丢帧 {r.dropped}\n"
            f"路径: {r.output_path}"
        )
        if r.ffmpeg_error:
            msg += f"\n⚠ {r.ffmpeg_error[:200]}"
        self.status.set(msg)

    # ------------------------------------------------------------------ #
    def _refresh_exclusion(self) -> None:
        """重建窗口捕获排除(修复隐藏/显示后 DWM 排除面失效)。

        现象: 录制中隐藏预览再显示 -> 预览窗口在画面里变成黑框。
        原因: hide/show 后 DWM 排除面失效, 亲和力标志仍为 17; 而重复设置
        相同值(17->17)是 no-op, 必须先置 0 再置 17(状态改变)才会强制
        重建排除面 —— 等效用户手动"取消再勾选"。
        """
        try:
            if not bool(self.exclude_var.get()):
                return
            set_capture_exclusion(self.root, False)
            set_capture_exclusion(self.root, True)
            if getattr(self, "preview_win", None) and self.preview_win.winfo_exists():
                set_capture_exclusion(self.preview_win, False)
                set_capture_exclusion(self.preview_win, True)
        except Exception:
            pass

    def _apply_exclusion(self) -> None:
        """按开关状态设置窗口捕获排除(Win10 2004+)。两个窗口都设。"""
        enable = bool(self.exclude_var.get())
        ok1 = set_capture_exclusion(self.root, enable)
        ok2 = True
        if getattr(self, "preview_win", None) and self.preview_win.winfo_exists():
            ok2 = set_capture_exclusion(self.preview_win, enable)
        if enable and not (ok1 and ok2):
            self.status.set("⚠ 系统不支持「不录制本软件窗口」(需 Win10 2004+)")
            self.exclude_var.set(False)
        elif enable:
            self.status.set("已排除本软件窗口: 预览/录制都不含本软件画面")
        self._save_cfg()

    # ------------------------------------------------------------------ #
    def _apply_window_icon(self) -> None:
        """窗口映射后设置标题栏/任务栏图标(iconbitmap 需映射后才生效)。"""
        try:
            if not getattr(self, "_icon_path", ""):
                return
            self.root.iconbitmap(self._icon_path)
        except Exception:
            pass

    def _on_status(self, st: dict) -> None:
        # 注意: 本回调在捕获线程中执行, 不能调用任何 Tk API
        # (否则会 "main thread is not in main loop")。只存数据,
        # 由 _poll_preview(Tk 主循环) 读取显示。
        with self._frame_lock:
            self._latest_status = st

    def _on_close(self) -> None:
        try:
            self._save_cfg()
            if getattr(self, "preview_win", None) and self.preview_win.winfo_exists():
                self._save_preview_geom()
                self.preview_win.destroy()
            if self.recorder:
                self.recorder.shutdown()
        finally:
            self.root.destroy()


def _gcd(a: int, b: int) -> int:
    """最大公约数(用于比例显示约分)。"""
    while b:
        a, b = b, a % b
    return max(1, a)


def _elapsed(start: float) -> float:
    import time
    return max(0.0, time.monotonic() - start)


def run_gui(ffmpeg: str, hw_encoder: str | None, on_ready=None) -> RecorderGUI:
    """启动 Tk 界面。on_ready(gui) 在 mainloop 前调用(用于挂热键)。"""
    root = tk.Tk()
    gui = RecorderGUI(root, ffmpeg, hw_encoder)
    if on_ready:
        on_ready(gui)
    root.mainloop()
    return gui
