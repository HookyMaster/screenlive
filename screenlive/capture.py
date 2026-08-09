"""Win32 高性能屏幕捕获: BitBlt 直接抓指定 region, 输出 BGRA numpy 帧。

为什么不用 mss: 实测 mss 在 4K 屏抓 810x1440 只有约 30fps 上限, 编码开销一
叠就掉到 11fps。直接用 GDI BitBlt 可达 60+ fps, 且每帧少一次 ScreenShot
对象封装与内部拷贝。
"""

from __future__ import annotations

import ctypes
from ctypes import wintypes

import numpy as np

__all__ = ["ScreenGrabber"]

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

SRCCOPY = 0x00CC0020
CAPTUREBLT = 0x40000000  # 包含分层窗口(半透明窗口)


class ScreenGrabber:
    """按 region 抓取屏幕, 返回 BGRA uint8 (H, W, 4) numpy 数组。

    用法:
        with ScreenGrabber() as g:
            frame = g.grab(left, top, width, height)
    """

    def __init__(self) -> None:
        self._hdc_screen: int | None = None
        self._hdc_mem: int | None = None
        self._hbmp: int | None = None
        self._buf: np.ndarray | None = None
        self._buf_key: tuple[int, int] = (0, 0)

    def __enter__(self) -> "ScreenGrabber":
        self._hdc_screen = user32.GetDC(0)
        self._hdc_mem = gdi32.CreateCompatibleDC(self._hdc_screen)
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def close(self) -> None:
        if self._hbmp:
            gdi32.DeleteObject(self._hbmp)
            self._hbmp = None
        if self._hdc_mem:
            gdi32.DeleteDC(self._hdc_mem)
            self._hdc_mem = None
        if self._hdc_screen:
            user32.ReleaseDC(0, self._hdc_screen)
            self._hdc_screen = None

    def grab(self, left: int, top: int, width: int, height: int) -> np.ndarray:
        """抓取屏幕矩形区域, 返回 BGRA 数组(行已按 DWORD 对齐)。"""
        w = int(width) & ~1
        h = int(height) & ~1
        if w <= 0 or h <= 0:
            raise ValueError(f"invalid size {width}x{height}")

        # 尺寸变化时重建 DIB(窗口尺寸档位切换才发生)
        if (w, h) != self._buf_key or self._hbmp is None:
            if self._hbmp:
                gdi32.DeleteObject(self._hbmp)
            bmi = _make_bmi(w, h)
            ptr = ctypes.c_void_p()
            self._hbmp = gdi32.CreateDIBSection(
                self._hdc_mem,
                ctypes.byref(bmi),
                0,  # DIB_RGB_COLORS
                ctypes.byref(ptr),
                0, 0,
            )
            if not self._hbmp:
                raise RuntimeError("CreateDIBSection failed")
            gdi32.SelectObject(self._hdc_mem, self._hbmp)
            self._buf = np.ctypeslib.as_array(
                ctypes.cast(ptr, ctypes.POINTER(ctypes.c_uint8)),
                shape=(h * w * 4,),
            )
            self._buf_key = (w, h)

        # BitBlt: 屏幕 -> 内存 DC (一次拷贝即得到像素)
        if not gdi32.BitBlt(
            self._hdc_mem, 0, 0, w, h,
            self._hdc_screen, left, top,
            SRCCOPY | CAPTUREBLT,
        ):
            raise RuntimeError(f"BitBlt failed (err={ctypes.GetLastError()})")

        # bmiHeader.biHeight 为负数 => DIB 本身就是顶向下的, 直接 reshape
        frame = self._buf.reshape(h, w, 4).copy()
        return frame


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", ctypes.c_long),
        ("biHeight", ctypes.c_long),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", ctypes.c_long),
        ("biYPelsPerMeter", ctypes.c_long),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [
        ("bmiHeader", BITMAPINFOHEADER),
        ("bmiColors", wintypes.DWORD * 3),
    ]


def _make_bmi(w: int, h: int) -> BITMAPINFO:
    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = w
    bmi.bmiHeader.biHeight = -h  # 负数 = 顶向下
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = 0  # BI_RGB
    return bmi
