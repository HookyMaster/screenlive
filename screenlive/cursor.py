"""轻量鼠标光标渲染 + 点击特效。

- 光标模板启动时预渲染为小尺寸 RGBA numpy 数组(预乘 alpha, 无彩边)。
- 点击特效: pynput 全局监听鼠标左/右/中键与滚轮, 在点击位置画外扩
  圆环动画(约 0.35s 淡出): 左键=红, 右键=蓝, 中键=黄, 滚轮=绿(带方向)。
- 每帧只需切片拷贝 + 一次乘加, 开销约几十微秒, 对性能影响可忽略。
- 不依赖任何图像库, 纯 numpy 实现。
"""

from __future__ import annotations

import threading
import time

import numpy as np

__all__ = ["CursorRenderer"]

#: 点击特效配色(BGR, 帧是 BGRA): 左键红 / 右键蓝 / 中键黄 / 滚轮绿
_FX_COLORS = {
    "left": (60, 60, 255),
    "right": (255, 120, 60),
    "middle": (60, 200, 255),
    "scroll_up": (120, 255, 60),
    "scroll_down": (60, 255, 200),
}
_FX_DURATION = 0.35   # 秒
_FX_R0 = 9.0          # 起始半径
_FX_R1 = 28.0         # 结束半径
_FX_WIDTH = 2.0       # 圆环宽度

#: 鼠标拖尾(轨迹): 历史点最旧->最新, 旧点小且淡
_TRAIL_LEN = 14       # 历史点数(30fps 下约 0.47s)
_TRAIL_R0 = 2.5       # 最旧点半径
_TRAIL_R1 = 6.0       # 最新点半径
_TRAIL_ALPHA0 = 0.15  # 最旧点透明度
_TRAIL_ALPHA1 = 0.85  # 最新点透明度
_TRAIL_COLOR = (60, 220, 255)  # BGR 亮黄(醒目)


def _scanline_fill(size: int, polygon: list[tuple[float, float]]) -> np.ndarray:
    """扫描线算法填充多边形, 返回 size x size 的 bool mask。"""
    mask = np.zeros((size, size), dtype=bool)
    n = len(polygon)
    for yy in range(size):
        yf = float(yy) + 0.5
        xs: list[float] = []
        for i in range(n):
            x1, y1 = polygon[i]
            x2, y2 = polygon[(i + 1) % n]
            if (y1 <= yf < y2) or (y2 <= yf < y1):
                xs.append(x1 + (yf - y1) * (x2 - x1) / (y2 - y1))
        if len(xs) >= 2:
            xs.sort()
            for j in range(0, len(xs) - 1, 2):
                a, b = int(np.ceil(xs[j])), int(np.floor(xs[j + 1]))
                mask[yy, max(a, 0): b + 1] = True
    return mask


def _dilate(mask: np.ndarray) -> np.ndarray:
    """3x3 十字膨胀, 用于生成描边。"""
    out = mask.copy()
    out[1:, :] |= mask[:-1, :]
    out[:-1, :] |= mask[1:, :]
    out[:, 1:] |= mask[:, :-1]
    out[:, :-1] |= mask[:, 1:]
    return out


def _make_arrow(size: int = 32) -> np.ndarray:
    """绘制经典箭头光标, 返回 RGBA uint8(热点在 (3,3) 附近)。"""
    # 箭头主体(白色) 与 描边多边形(黑色)
    body = [
        (3.0, 3.0), (25.0, 9.5), (15.5, 15.5), (20.5, 27.5), (3.0, 3.0),
    ]
    # 外轮廓 = 主体放大偏移, 作为黑描边
    cx, cy = 12.0, 14.0
    outline = [(x + (x - cx) * 0.45, y + (y - cy) * 0.45) for x, y in body]

    white = _scanline_fill(size, body)
    black = _scanline_fill(size, outline)
    border = black & ~white

    rgba = np.zeros((size, size, 4), dtype=np.uint8)
    rgba[white, 0] = 255  # R
    rgba[white, 1] = 255  # G
    rgba[white, 2] = 255  # B
    rgba[white, 3] = 255  # A
    rgba[border, 0:3] = 0  # 黑描边
    rgba[border, 3] = 255
    return rgba


class CursorRenderer:
    """预渲染光标 + 点击特效叠加到 BGRA 帧上。"""

    def __init__(
        self,
        arrow_size: int = 32,
        hotspot: tuple[int, int] = (3, 3),
        show_cursor: bool = True,
        show_fx: bool = True,
        show_trail: bool = True,
    ) -> None:
        arrow = _make_arrow(arrow_size)
        h, w = arrow.shape[:2]
        # 预乘 alpha, 避免边缘色边
        a = arrow[..., 3:4].astype(np.float32) / 255.0
        premul = arrow[..., :3].astype(np.float32) * a
        # 转 BGRA 顺序, 与帧格式一致
        self._bgra = np.empty((h, w, 4), dtype=np.float32)
        self._bgra[..., 0] = premul[..., 2]  # B
        self._bgra[..., 1] = premul[..., 1]  # G
        self._bgra[..., 2] = premul[..., 0]  # R
        self._bgra[..., 3] = a[..., 0]       # A
        self._h, self._w = h, w
        self._hx, self._hy = hotspot

        self.show_cursor = show_cursor
        self.show_fx = show_fx
        self.show_trail = show_trail
        # 点击特效: 屏幕坐标 + 触发时间 + 类型(pynput 线程写, 渲染线程读)
        self._fx_lock = threading.Lock()
        self._effects: list[tuple[float, int, int, str]] = []
        self._listener = None
        # 鼠标拖尾: (屏幕x, 屏幕y) 历史点(渲染线程写)
        self._trail: list[tuple[float, float]] = []
        if show_fx:
            self._start_listener()

    # ------------------------------------------------------------------ #
    # 全局鼠标监听(pynput)
    # ------------------------------------------------------------------ #
    def _start_listener(self) -> None:
        try:
            from pynput import mouse

            self._listener = mouse.Listener(
                on_click=self._on_click, on_scroll=self._on_scroll)
            self._listener.daemon = True
            self._listener.start()
        except Exception:
            self._listener = None

    def _on_click(self, x, y, button, pressed) -> None:
        if not pressed:
            return
        name = getattr(button, "name", str(button))
        if name == "left":
            key = "left"
        elif name == "right":
            key = "right"
        elif name in ("middle", "unknown"):  # 中键
            key = "middle"
        else:
            return
        with self._fx_lock:
            self._effects.append((time.monotonic(), int(x), int(y), key))

    def _on_scroll(self, x, y, dx, dy) -> None:
        key = "scroll_up" if dy > 0 else "scroll_down"
        with self._fx_lock:
            self._effects.append((time.monotonic(), int(x), int(y), key))

    def stop(self) -> None:
        """停止全局鼠标监听(recorder 关闭时调用)。"""
        if self._listener is not None:
            try:
                self._listener.stop()
            except Exception:
                pass
            self._listener = None

    # ------------------------------------------------------------------ #
    # 渲染
    # ------------------------------------------------------------------ #
    @property
    def size(self) -> tuple[int, int]:
        return self._h, self._w

    def draw(
        self,
        frame: np.ndarray,
        mouse_rel_x: int,
        mouse_rel_y: int,
        region_abs: tuple[int, int] | None = None,
        kx: float = 1.0,
        ky: float = 1.0,
    ) -> None:
        """在 frame(BGRA uint8, HxWx4) 上绘制光标、点击特效与鼠标拖尾。

        mouse_rel_x/y: 鼠标在帧内的坐标(热点)。
        region_abs: 捕获区域的绝对屏幕左上角(用于把屏幕坐标换算到帧内);
                    动态视野缩放时 kx/ky 为缩放比例。
        """
        if self.show_trail:
            self._draw_trail(frame, mouse_rel_x, mouse_rel_y,
                             region_abs, kx, ky)
        if self.show_cursor:
            self._draw_arrow(frame, mouse_rel_x, mouse_rel_y)
        if self.show_fx:
            self._draw_fx(frame, region_abs, kx, ky)

    # ------------------------------------------------------------------ #
    # 鼠标拖尾
    # ------------------------------------------------------------------ #
    def _draw_trail(
        self,
        frame: np.ndarray,
        mouse_rel_x: float,
        mouse_rel_y: float,
        region_abs: tuple[int, int] | None,
        kx: float,
        ky: float,
    ) -> None:
        """鼠标移动留下渐隐拖尾: 当前点入队, 历史点画渐隐小圆点。"""
        if region_abs is None:
            return
        # 当前鼠标屏幕坐标 = 区域左上 + 帧内坐标 / 缩放比
        sx = region_abs[0] + mouse_rel_x / kx if kx else region_abs[0]
        sy = region_abs[1] + mouse_rel_y / ky if ky else region_abs[1]
        self._trail.append((float(sx), float(sy)))
        if len(self._trail) > _TRAIL_LEN:
            self._trail.pop(0)

        n = len(self._trail)
        for i, (px, py) in enumerate(self._trail):
            # 帧内坐标
            cx = (px - region_abs[0]) * kx
            cy = (py - region_abs[1]) * ky
            # 越旧的越小越淡
            t = i / max(1, n - 1)
            radius = _TRAIL_R0 + (_TRAIL_R1 - _TRAIL_R0) * t
            alpha = _TRAIL_ALPHA0 + (_TRAIL_ALPHA1 - _TRAIL_ALPHA0) * t
            if alpha < 0.02:
                continue
            self._draw_dot(frame, cx, cy, radius, _TRAIL_COLOR, alpha)

    @staticmethod
    def _draw_dot(
        frame: np.ndarray,
        cx: float,
        cy: float,
        radius: float,
        color: tuple[int, int, int],
        alpha: float,
    ) -> None:
        """画一个实心圆点(距离场, 阈值混合)。"""
        h, w = frame.shape[:2]
        pad = int(radius) + 2
        x0, y0 = max(0, int(cx - pad)), max(0, int(cy - pad))
        x1, y1 = min(w, int(cx + pad)), min(h, int(cy + pad))
        if x1 <= x0 or y1 <= y0:
            return
        yy, xx = np.mgrid[y0:y1, x0:x1]
        d = np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2)
        mask = d <= radius
        if not mask.any():
            return
        roi = frame[y0:y1, x0:x1]
        rgb = roi[..., :3][mask].astype(np.float32)
        rgb *= (1.0 - alpha)
        rgb += np.asarray(color, dtype=np.float32) * alpha
        roi[..., :3][mask] = rgb.astype(np.uint8)

    def _draw_arrow(self, frame: np.ndarray, mx: int, my: int) -> None:
        h, w = frame.shape[:2]
        x0 = int(mx) - self._hx
        y0 = int(my) - self._hy
        x1 = x0 + self._w
        y1 = y0 + self._h
        if x1 <= 0 or y1 <= 0 or x0 >= w or y0 >= h:
            return
        sx0 = max(0, -x0)
        sy0 = max(0, -y0)
        dx0 = max(0, x0)
        dy0 = max(0, y0)
        sx1 = sx0 + (min(w, x1) - dx0)
        sy1 = sy0 + (min(h, y1) - dy0)
        if sx1 <= sx0 or sy1 <= sy0:
            return
        cursor = self._bgra[sy0:sy1, sx0:sx1]
        roi = frame[dy0:dy0 + sy1 - sy0, dx0:dx0 + sx1 - sx0]
        rgb = roi[..., :3]
        alpha = cursor[..., 3:4]
        inv_a = np.float32(1.0) - alpha
        np.multiply(rgb, inv_a, out=rgb, casting="unsafe")
        np.add(rgb, cursor[..., :3], out=rgb, casting="unsafe")

    def _draw_fx(
        self,
        frame: np.ndarray,
        region_abs: tuple[int, int] | None,
        kx: float,
        ky: float,
    ) -> None:
        """绘制活跃的点击特效(外扩圆环, 0.35s 淡出)。"""
        now = time.monotonic()
        with self._fx_lock:
            active = [e for e in self._effects]
            self._effects.clear()
        for t0, ex, ey, key in active:
            age = now - t0
            if age < 0 or age >= _FX_DURATION:
                continue
            self._effects.append((t0, ex, ey, key))  # 未过期, 继续保留
            if region_abs is None:
                continue
            cx = (ex - region_abs[0]) * kx
            cy = (ey - region_abs[1]) * ky
            if cx < -40 or cy < -40 or cx > frame.shape[1] + 40 or cy > frame.shape[0] + 40:
                continue
            t = age / _FX_DURATION
            radius = _FX_R0 + (_FX_R1 - _FX_R0) * t
            alpha = float(1.0 - t)
            self._draw_ring(frame, cx, cy, radius, _FX_COLORS[key], alpha)
            self._draw_direction(frame, cx, cy, key, alpha)

    @staticmethod
    def _draw_ring(
        frame: np.ndarray,
        cx: float,
        cy: float,
        radius: float,
        color: tuple[int, int, int],
        alpha: float,
    ) -> None:
        h, w = frame.shape[:2]
        pad = int(radius) + 3
        x0, y0 = max(0, int(cx - pad)), max(0, int(cy - pad))
        x1, y1 = min(w, int(cx + pad)), min(h, int(cy + pad))
        if x1 <= x0 or y1 <= y0:
            return
        yy, xx = np.mgrid[y0:y1, x0:x1]
        d = np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2)
        ring = np.abs(d - radius) <= _FX_WIDTH
        if not ring.any():
            return
        roi = frame[y0:y1, x0:x1]
        # 注意: 布尔索引返回副本, 需计算后赋值写回
        rgb = roi[..., :3][ring].astype(np.float32)
        rgb *= (1.0 - alpha)
        rgb += np.asarray(color, dtype=np.float32) * alpha
        roi[..., :3][ring] = rgb.astype(np.uint8)

    @staticmethod
    def _draw_direction(
        frame: np.ndarray,
        cx: float,
        cy: float,
        key: str,
        alpha: float,
    ) -> None:
        """滚轮效果: 在圆环上方/下方画 2px 短线指示滚动方向。"""
        if key not in ("scroll_up", "scroll_down"):
            return
        dy = -6.0 if key == "scroll_up" else 6.0
        color = _FX_COLORS[key]
        h, w = frame.shape[:2]
        x, y = int(round(cx)), int(round(cy + dy))
        if 0 <= x < w and 0 <= y - 1 < h and y + 1 >= 0:
            ys = max(0, y - 1)
            ye = min(h, y + 2)
            frame[ys:ye, x, :3] = (
                frame[ys:ye, x, :3] * (1 - alpha)
                + np.asarray(color, dtype=np.float32) * alpha
            ).astype(np.uint8)
