"""鼠标追踪与裁剪窗口计算。

设计要点:
- 支持常用比例预设(9:16 / 16:9 / 1:1 / 3:4 / 4:3 / 9:21)以及自定义任意像素。
- 像素尺寸任意指定, 自动对齐偶数(h264/yuv420p 要求), 超出屏幕时按比例缩放适配。
- 鼠标尽量保持画面中间: EMA 强跟随(smooth 越大越跟手, 1.0=始终居中)。
- 窗口碰到屏幕边缘时尺寸保持不变, 仅 clamp 位置(鼠标自然偏离中心)。
"""

from __future__ import annotations

import ctypes
from ctypes import Structure, byref, c_long

__all__ = [
    "get_cursor_pos", "set_dpi_aware", "BoxTracker", "NineSixteenTracker",
    "RATIO_PRESETS", "SIZE_PRESETS",
]

#: 常用比例预设: 名称 -> (宽比, 高比)。"自定义" 表示像素完全自由。
RATIO_PRESETS: dict[str, tuple[int, int]] = {
    "9:16 (竖屏手机)": (9, 16),
    "16:9 (横屏)": (16, 9),
    "1:1 (方形)": (1, 1),
    "3:4": (3, 4),
    "4:3": (4, 3),
    "9:21 (长屏)": (9, 21),
    "自定义": (0, 0),
}

#: 兼容保留: 9:16 高度档位
SIZE_PRESETS: dict[str, int] = {
    "大 (1080x1920)": 1920,
    "中 (810x1440)": 1440,
    "小 (608x1080)": 1080,
}


def set_dpi_aware() -> None:
    """让进程按物理像素工作, 保证 GetCursorPos 与抓屏坐标一致(win32)。

    注意: 使用 SYSTEM 级(1)而非 PER-MONITOR 级(2) —— Tk 8.6 在 per-monitor
    模式下窗口尺寸计算会错乱, system 级下坐标同样是物理像素, 且 Tk 能按
    系统 DPI 正确缩放界面。
    """
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(1)  # System DPI aware
    except Exception:
        try:
            ctypes.windll.user32.SetProcessDPIAware()
        except Exception:
            pass


class _POINT(Structure):
    _fields_ = [("x", c_long), ("y", c_long)]


def get_cursor_pos() -> tuple[int, int]:
    """获取全局鼠标位置(虚拟桌面物理像素坐标)。跨平台降级: 尽力而为。"""
    try:
        pt = _POINT()
        ctypes.windll.user32.GetCursorPos(byref(pt))
        return int(pt.x), int(pt.y)
    except Exception:
        return 0, 0


class BoxTracker:
    """计算裁剪窗口并平滑跟随鼠标(窗口居中于鼠标)。支持任意目标像素。

    动态视野(zoom): 鼠标大幅滑动时按速度放大捕获区域(收进更多画面),
    鼠标速度降下来后平滑回到基准尺寸; 输出分辨率不变, 捕获区域大小时
    由调用方(recorder)缩放回输出尺寸。

    参数:
        screen_w/screen_h: 屏幕(虚拟桌面)尺寸
        target_w/target_h: 期望输出像素(宽高), 超出屏幕会按比例缩放适配
        smooth: 跟随系数, 1.0=完全跟手(鼠标始终居中), 越小越稳
        zoom_thresh: 鼠标速度阈值(像素/帧), 超过才触发放大
        zoom_k: 速度 -> 缩放系数的增益(每超出阈值 1px 放大 zoom_k 倍)
        zoom_max: 缩放上限(倍), 同时受屏幕尺寸限制
    """

    def __init__(
        self,
        screen_w: int,
        screen_h: int,
        target_w: int = 810,
        target_h: int = 1440,
        smooth: float = 0.85,
        zoom_thresh: float = 40.0,
        zoom_k: float = 0.03,
        zoom_max: float = 3.0,
    ) -> None:
        self.screen_w = int(screen_w)
        self.screen_h = int(screen_h)
        self.base_w, self.base_h = self._fit(int(target_w), int(target_h))
        self.win_w, self.win_h = self.base_w, self.base_h

        self.smooth = max(0.0, min(1.0, smooth))
        # 当前窗口左上角(平滑值), 初始屏幕中心
        self._cur_x = float(max(0, (self.screen_w - self.win_w) // 2))
        self._cur_y = float(max(0, (self.screen_h - self.win_h) // 2))

        # ---- 动态视野(zoom) ----
        self.zoom_thresh = float(zoom_thresh)
        self.zoom_k = float(zoom_k)
        # 放大上限: 不超过屏幕尺寸与设定上限
        self.zoom_max = min(
            float(zoom_max),
            self.screen_w / self.base_w,
            self.screen_h / self.base_h,
        )
        self.zoom_max = max(1.0, self.zoom_max)
        self._scale = 1.0            # 当前缩放系数(平滑值)
        # 放大平滑(较快到达目标视野, 让放大效果明显)、恢复平滑快(快速回基准)
        self._scale_smooth_up = 0.35
        self._scale_smooth_down = 0.5
        # 鼠标速度 EMA: 滤掉单帧抖动, 只有持续快速滑动才触发放大
        self._speed_ema = 0.0
        self._speed_k = 0.4
        self._prev_mx: int | None = None
        self._prev_my: int | None = None

    def _fit(self, w: int, h: int) -> tuple[int, int]:
        """把目标尺寸适配到屏幕内(保持比例), 并对齐偶数、下限 2px。"""
        w = max(2, int(w))
        h = max(2, int(h))
        scale = min(self.screen_w / w, self.screen_h / h, 1.0)
        if scale < 1.0:
            w = int(w * scale)
            h = int(h * scale)
        return max(2, w & ~1), max(2, h & ~1)

    def set_size(self, target_w: int, target_h: int) -> None:
        """运行时更新目标尺寸(保持当前位置不跳动), 并复位缩放。"""
        self.base_w, self.base_h = self._fit(int(target_w), int(target_h))
        self.win_w, self.win_h = self.base_w, self.base_h
        self._scale = 1.0
        self._prev_mx = self._prev_my = None
        self.zoom_max = min(
            self.zoom_max, self.screen_w / self.base_w, self.screen_h / self.base_h)
        self.zoom_max = max(1.0, self.zoom_max)

    @property
    def size(self) -> tuple[int, int]:
        return self.win_w, self.win_h

    @property
    def scale(self) -> float:
        """当前视野缩放系数(1.0 = 基准尺寸)。"""
        return self._scale

    def _update_scale(self, mouse_x: int, mouse_y: int) -> None:
        """按鼠标速度更新视野缩放: 持续大幅滑动才放大, 慢下来平滑回基准。

        速度先做 EMA 平滑(滤掉单帧抖动), 超过 zoom_thresh 才触发放大;
        放大平滑慢、恢复平滑快, 避免小幅移动/停顿时的缩放残留感。
        """
        if self._prev_mx is None:
            v = 0.0
        else:
            v = ((mouse_x - self._prev_mx) ** 2
                 + (mouse_y - self._prev_my) ** 2) ** 0.5
        self._prev_mx, self._prev_my = mouse_x, mouse_y
        self._speed_ema = self._speed_ema * (1.0 - self._speed_k) + v * self._speed_k
        v = self._speed_ema

        if v > self.zoom_thresh:
            target = 1.0 + (v - self.zoom_thresh) * self.zoom_k
            target = min(target, self.zoom_max)
        else:
            target = 1.0
        k = self._scale_smooth_up if target >= self._scale else self._scale_smooth_down
        self._scale += (target - self._scale) * k
        if abs(self._scale - 1.0) < 0.001:
            self._scale = 1.0

    def update(self, mouse_x: int, mouse_y: int) -> dict[str, int]:
        """按鼠标位置更新裁剪窗口, 返回 region 字典(left/top/width/height)。

        鼠标位于窗口中心; 窗口越界时 clamp 位置但保持尺寸不变。
        鼠标速度决定视野缩放(见 _update_scale)。
        """
        self._update_scale(mouse_x, mouse_y)

        # 当前窗口尺寸 = 基准 x 缩放(对齐偶数)
        self.win_w = max(2, round(self.base_w * self._scale)) & ~1
        self.win_h = max(2, round(self.base_h * self._scale)) & ~1
        self.win_w = min(self.win_w, self.screen_w)
        self.win_h = min(self.win_h, self.screen_h)

        tlx = mouse_x - self.win_w / 2
        tly = mouse_y - self.win_h / 2
        tlx = max(0.0, min(tlx, float(self.screen_w - self.win_w)))
        tly = max(0.0, min(tly, float(self.screen_h - self.win_h)))

        k = self.smooth
        self._cur_x += (tlx - self._cur_x) * k
        self._cur_y += (tly - self._cur_y) * k

        left = round(self._cur_x)
        top = round(self._cur_y)
        left = max(0, min(left, self.screen_w - self.win_w))
        top = max(0, min(top, self.screen_h - self.win_h))

        return {"left": left, "top": top, "width": self.win_w, "height": self.win_h}


class NineSixteenTracker(BoxTracker):
    """兼容旧接口: 固定 9:16, 由目标高度推算宽度。"""

    def __init__(
        self,
        screen_w: int,
        screen_h: int,
        target_h: int = 1440,
        smooth: float = 0.85,
    ) -> None:
        h = int(target_h)
        w = round(h * 9 / 16)
        super().__init__(screen_w, screen_h, w, h, smooth)
