"""录屏核心: 常驻捕获线程 + 实时帧回调 + 录制时才启动编码器。

关键设计:
1. **常驻捕获**: 应用打开即启动捕获线程, 持续抓取鼠标周围 9:16 区域,
   通过 on_frame 回调把每帧推给 GUI 做实时预览(不录制也能看到选框)。
2. **录制解耦**: 只有点击"开始录制"才启动 ffmpeg 编码线程; 停止后编码进程
   立即关闭, 但捕获/预览继续运行。
3. **BitBlt 直接抓屏**: 绕过 mss 的封装开销, 810x1440 实测 33+ fps,
   只截小窗口零缩放, CPU 开销最低。
4. **有界队列 + 丢帧**: 编码跟不上时主动丢帧保实时, 队列最多积压 2 秒。
5. **隐藏子进程窗口**: ffmpeg 以 CREATE_NO_WINDOW 启动, 不弹控制台。
"""

from __future__ import annotations

import os
import queue
import subprocess
import threading
import time
from typing import Callable, Optional

import numpy as np

from .capture import ScreenGrabber
from .cursor import CursorRenderer
from .encoder import EncSpec, build_command
from .tracker import BoxTracker, get_cursor_pos

__all__ = ["ScreenRecorder"]

# Windows 下隐藏子进程控制台窗口
_NO_WINDOW = 0x08000000 if os.name == "nt" else 0


def _virtual_screen() -> tuple[int, int, int, int]:
    """虚拟屏幕(所有显示器合并)的 left, top, width, height。"""
    import ctypes
    u = ctypes.windll.user32
    x = u.GetSystemMetrics(76)  # SM_XVIRTUALSCREEN
    y = u.GetSystemMetrics(77)  # SM_YVIRTUALSCREEN
    w = u.GetSystemMetrics(78)  # SM_CXVIRTUALSCREEN
    h = u.GetSystemMetrics(79)  # SM_CYVIRTUALSCREEN
    if w <= 0 or h <= 0:  # 非 win32 或异常: 退回主屏
        return 0, 0, u.GetSystemMetrics(0), u.GetSystemMetrics(1)
    return x, y, w, h


class ScreenRecorder:
    """常驻捕获 + 可选录制。

    参数:
        ffmpeg:      ffmpeg 可执行路径
        fps:         捕获/录制帧率
        target_w/h:  输出视频像素(宽/高), 任意值; 超出屏幕按比例适配
        smooth:      跟随系数(1.0 完全跟手, 鼠标尽量居中)
        show_cursor: 帧上是否叠加鼠标光标
        on_frame:    每帧回调 fn(bgra_frame, region, mouse_rel) -> None, 供预览
        on_status:   录制状态回调 fn(dict) -> None
    """

    def __init__(
        self,
        ffmpeg: str,
        fps: int = 30,
        target_w: int = 810,
        target_h: int = 1440,
        smooth: float = 0.85,
        show_cursor: bool = True,
        on_frame: Optional[Callable[[np.ndarray, dict, tuple], None]] = None,
        on_status: Optional[Callable[[dict], None]] = None,
    ) -> None:
        self.ffmpeg = ffmpeg
        self.fps = max(1, int(fps))
        self.target_w = int(target_w)
        self.target_h = int(target_h)
        self.smooth = smooth
        self.show_cursor = show_cursor
        self.on_frame = on_frame
        self.on_status = on_status

        self._frames: queue.Queue = queue.Queue(maxsize=self.fps * 2)
        self._running = False          # 常驻捕获开关
        self._recording = False        # 录制开关
        self._capture_done = threading.Event()
        self._capture_thread: Optional[threading.Thread] = None
        self._encode_thread: Optional[threading.Thread] = None
        self._proc: Optional[subprocess.Popen] = None
        self._tracker: Optional[BoxTracker] = None
        # 待应用的尺寸变更(线程间安全: 主线程写, 捕获线程读)
        self._pending_size: Optional[tuple[int, int]] = None

        self.output_path = ""
        self.enc: Optional[EncSpec] = None

        # 统计
        self.captured = 0
        self.encoded = 0
        self.dropped = 0
        self.duplicated = 0
        self.start_time = 0.0
        self.ffmpeg_error: str = ""

    # ------------------------------------------------------------------ #
    # 常驻捕获
    # ------------------------------------------------------------------ #
    def start(self) -> None:
        """启动常驻捕获线程(开始抓帧, 供预览; 不录制)。"""
        if self._running:
            return
        self._running = True
        self._capture_done.clear()
        self._capture_thread = threading.Thread(
            target=self._capture_loop, daemon=True
        )
        self._capture_thread.start()

    def shutdown(self) -> None:
        """彻底停止捕获线程(应用退出时调用)。"""
        if self._recording:
            self.stop_recording()
        self._running = False
        if self._capture_thread:
            self._capture_thread.join(timeout=3.0)

    # ------------------------------------------------------------------ #
    # 录制启停
    # ------------------------------------------------------------------ #
    def start_recording(self, output_path: str, enc: EncSpec) -> None:
        if not self._running:
            self.start()
        self.output_path = output_path
        self.enc = enc
        self.encoded = 0
        self.dropped = 0
        self.duplicated = 0
        self.captured = 0
        self.ffmpeg_error = ""
        self.start_time = time.monotonic()
        # 清空队列残留
        while not self._frames.empty():
            try:
                self._frames.get_nowait()
            except queue.Empty:
                break
        self._capture_done.clear()
        self._recording = True
        self._encode_thread = threading.Thread(
            target=self._encode_loop, daemon=True
        )
        self._encode_thread.start()

    def stop_recording(self) -> None:
        if not self._recording:
            return
        self._recording = False
        # 等待编码线程把队列中剩余帧写完(最长 10s), 避免结尾丢帧
        deadline = time.monotonic() + 10.0
        while self._encode_thread and self._encode_thread.is_alive() \
                and time.monotonic() < deadline:
            self._encode_thread.join(timeout=0.2)
            if self._frames.empty():
                self._encode_thread.join(timeout=3.0)
                break

    @property
    def is_running(self) -> bool:
        return self._running

    @property
    def is_recording(self) -> bool:
        return self._recording

    def set_size(self, target_w: int, target_h: int) -> bool:
        """请求变更输出尺寸。录制中不允许(帧尺寸必须一致), 返回是否接受。"""
        if self._recording:
            return False
        self.target_w = int(target_w)
        self.target_h = int(target_h)
        self._pending_size = (self.target_w, self.target_h)
        return True

    # ------------------------------------------------------------------ #
    # 捕获线程(常驻)
    # ------------------------------------------------------------------ #
    def _capture_loop(self) -> None:
        # 提升 Windows 定时器精度到 1ms, 保证帧率节奏准确
        try:
            import ctypes
            ctypes.windll.winmm.timeBeginPeriod(1)
        except Exception:
            pass

        cursor = CursorRenderer() if self.show_cursor else None
        frame_dt = 1.0 / self.fps
        next_tick = time.monotonic()
        last_status = 0.0

        try:
            vx, vy, vw, vh = _virtual_screen()
            tracker = BoxTracker(
                vw, vh, self.target_w, self.target_h, self.smooth
            )
            self._tracker = tracker

            with ScreenGrabber() as grabber:
                while self._running:
                    now = time.monotonic()

                    # 应用主线程请求的尺寸变更(仅非录制时)
                    if self._pending_size is not None and not self._recording:
                        pw, ph = self._pending_size
                        self._pending_size = None
                        tracker.set_size(pw, ph)

                    mx, my = get_cursor_pos()
                    rel_x = mx - vx
                    rel_y = my - vy

                    region = tracker.update(rel_x, rel_y)
                    frame = grabber.grab(
                        vx + region["left"], vy + region["top"],
                        region["width"], region["height"],
                    )

                    mouse_rel = (rel_x - region["left"], rel_y - region["top"])

                    if cursor is not None:
                        cursor.draw(frame, mouse_rel[0], mouse_rel[1])

                    self.captured += 1

                    # 预览回调(常驻, 不录制也推帧)
                    if self.on_frame:
                        try:
                            self.on_frame(frame, region, mouse_rel)
                        except Exception:
                            pass

                    # 仅在录制时喂编码器
                    if self._recording:
                        try:
                            self._frames.put_nowait(frame)
                        except queue.Full:
                            self.dropped += 1  # 主动丢帧保实时

                    next_tick += frame_dt
                    delay = next_tick - time.monotonic()
                    if delay > 0:
                        time.sleep(delay)
                    else:
                        next_tick = time.monotonic()

                    if now - last_status >= 0.5 and self.on_status and self._recording:
                        last_status = now
                        self._emit_status()
        finally:
            try:
                import ctypes
                ctypes.windll.winmm.timeEndPeriod(1)
            except Exception:
                pass
            self._capture_done.set()

    # ------------------------------------------------------------------ #
    # 编码线程(仅录制时)
    # ------------------------------------------------------------------ #
    def _encode_loop(self) -> None:
        """按真实时间节奏写帧, 保证输出时长与真实时间一致。

        - 节奏时钟锚定到**第一帧实际到达**(而非录制启动时刻),
          首帧延迟(编码器预热/线程调度)不会吃掉录制时长;
        - 队列有新帧: 取最新一帧(丢弃积压旧帧, 保实时);
        - 队列为空但录制中: 重复上一帧(补帧), 避免视频"快进";
        - ffmpeg 以固定 fps 打时间戳, 因此写节奏必须严格 = fps。
        """
        frame_dt = 1.0 / self.fps
        proc: Optional[subprocess.Popen] = None
        last_frame: Optional[np.ndarray] = None
        next_tick: Optional[float] = None  # 首帧到达时才锚定

        try:
            while True:
                # 取队列中最新一帧(丢弃积压的旧帧保实时)
                frame = None
                try:
                    while True:
                        frame = self._frames.get_nowait()
                except queue.Empty:
                    pass

                if frame is not None:
                    last_frame = frame
                elif last_frame is None:
                    # 还没有任何帧
                    if not self._recording:
                        break  # 停止了也没收到帧
                    time.sleep(0.005)
                    continue
                elif not self._recording:
                    # 停止且队列已排空: 退出
                    break

                # 首帧到达: 锚定节奏时钟 + 录制计时起点(时长从这里算)
                if next_tick is None:
                    next_tick = time.monotonic()
                    self.start_time = next_tick

                # 等到本帧的节拍时刻
                delay = next_tick - time.monotonic()
                if delay > 0:
                    time.sleep(delay)
                next_tick += frame_dt
                # 落后太多(>10 帧): 重新对齐, 避免越拖越慢
                if time.monotonic() - next_tick > frame_dt * 10:
                    next_tick = time.monotonic() + frame_dt

                # 录制中但捕获没跟上: 重复上一帧补齐节奏, 保时长正确
                if frame is None:
                    self.duplicated += 1

                if proc is None:
                    h, w = last_frame.shape[:2]
                    cmd = build_command(
                        self.ffmpeg, w, h, self.fps, self.enc, self.output_path
                    )
                    proc = subprocess.Popen(
                        cmd,
                        stdin=subprocess.PIPE,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.PIPE,
                        bufsize=1024 * 1024,
                        creationflags=_NO_WINDOW,
                    )
                    self._proc = proc

                try:
                    proc.stdin.write(last_frame.tobytes())
                    self.encoded += 1
                except (BrokenPipeError, OSError):
                    break
        finally:
            if proc is not None:
                try:
                    proc.stdin.close()  # type: ignore[union-attr]
                except Exception:
                    pass
                try:
                    proc.wait(timeout=15)
                except Exception:
                    proc.kill()
                if proc.returncode not in (0, None):
                    try:
                        err = proc.stderr.read().decode(errors="replace")
                    except Exception:
                        err = ""
                    self.ffmpeg_error = (
                        f"ffmpeg 退出码 {proc.returncode}: {err[-800:]}"
                    )
            self._proc = None

    # ------------------------------------------------------------------ #
    def _emit_status(self) -> None:
        if not self.on_status:
            return
        elapsed = time.monotonic() - self.start_time
        self.on_status(
            {
                "captured": self.captured,
                "encoded": self.encoded,
                "dropped": self.dropped,
                "elapsed": elapsed,
                "fps": self.captured / elapsed if elapsed > 0 else 0.0,
            }
        )
