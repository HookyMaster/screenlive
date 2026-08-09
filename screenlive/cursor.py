"""轻量鼠标光标渲染。

- 光标模板启动时预渲染为小尺寸 RGBA numpy 数组(预乘 alpha, 无彩边)。
- 每帧只需切片拷贝 + 一次乘加, 开销约几十微秒, 对性能影响可忽略。
- 不依赖任何图像库, 纯 numpy 实现。
"""

from __future__ import annotations

import numpy as np

__all__ = ["CursorRenderer"]


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
    """把预渲染光标叠加到 BGRA 帧上。"""

    def __init__(self, arrow_size: int = 32, hotspot: tuple[int, int] = (3, 3)) -> None:
        arrow = _make_arrow(arrow_size)
        h, w = arrow.shape[:2]
        # 预乘 alpha, 避免边缘色边
        a = arrow[..., 3:4].astype(np.float32) / 255.0
        premul = arrow[..., :3].astype(np.float32) * a
        # 转 BGRA 顺序, 与 mss 帧格式一致
        self._bgra = np.empty((h, w, 4), dtype=np.float32)
        self._bgra[..., 0] = premul[..., 2]  # B
        self._bgra[..., 1] = premul[..., 1]  # G
        self._bgra[..., 2] = premul[..., 0]  # R
        self._bgra[..., 3] = a[..., 0]       # A
        self._h, self._w = h, w
        self._hx, self._hy = hotspot

    @property
    def size(self) -> tuple[int, int]:
        return self._h, self._w

    def draw(self, frame: np.ndarray, mouse_rel_x: int, mouse_rel_y: int) -> None:
        """在 frame(BGRA uint8, HxWx4) 上以 (mouse_rel_x, mouse_rel_y) 为热点绘制光标。"""
        h, w = frame.shape[:2]
        x0 = int(mouse_rel_x) - self._hx
        y0 = int(mouse_rel_y) - self._hy
        x1 = x0 + self._w
        y1 = y0 + self._h
        if x1 <= 0 or y1 <= 0 or x0 >= w or y0 >= h:
            return

        # 裁剪到帧内
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
        rgb = roi[..., :3]  # 3 通道视图, 就地修改即更新 frame
        alpha = cursor[..., 3:4]
        inv_a = np.float32(1.0) - alpha
        # 预乘 alpha 混合, 全程 float32, unsafe 写回 uint8(截断)
        np.multiply(rgb, inv_a, out=rgb, casting="unsafe")
        np.add(rgb, cursor[..., :3], out=rgb, casting="unsafe")
