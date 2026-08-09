"""FFmpeg 编码器封装。

优先使用 GPU 硬件编码(NVENC/QSV/AMF), 把 CPU 占用降到最低;
无可用硬件编码时回退 libx264。帧以 rawvideo(BGRA) 直接管道喂入,
编码进程写实时完成, 不产生任何中间文件, 也不占用磁盘。

注意: ffmpeg 的 -encoders 列表只说明"编译进了该编码器", 不代表本机
GPU/驱动能真正初始化它(如无 NVIDIA 显卡时 nvenc 会 CUDA_ERROR_NO_DEVICE)。
因此 detect_hardware_encoder 会用合成帧实测验证, 通过才算可用。
"""

from __future__ import annotations

import os
import shutil
import subprocess
from dataclasses import dataclass, field

__all__ = ["find_ffmpeg", "build_encoder_command", "detect_hardware_encoder",
           "verify_encoder", "EncSpec"]

# 编码质量档位 -> 各编码器的参数
_NVENC_PRESET = {"fast": "p1", "medium": "p4", "high": "p5"}
_NVENC_CQ = {"fast": 28, "medium": 24, "high": 21}
_X264_PRESET = {"fast": "ultrafast", "medium": "veryfast", "high": "fast"}
_X264_CRF = {"fast": 28, "medium": 25, "high": 21}

_HW_ENCODERS = ["h264_nvenc", "h264_qsv", "h264_amf", "h264_videotoolbox"]


@dataclass
class EncSpec:
    """一条编码器参数(FFmpeg args), 附带人类可读名称。"""

    name: str
    args: list[str] = field(default_factory=list)


def find_ffmpeg() -> str:
    """定位 ffmpeg: 优先系统安装, 否则使用 imageio-ffmpeg 内置二进制。"""
    path = shutil.which("ffmpeg")
    if path:
        return path
    try:
        import imageio_ffmpeg

        return imageio_ffmpeg.get_ffmpeg_exe()
    except Exception:
        raise RuntimeError(
            "未找到 ffmpeg。请安装系统 ffmpeg 或执行: pip install imageio-ffmpeg"
        )


def detect_hardware_encoder(ffmpeg: str) -> str | None:
    """探测真正可用的 h264 硬件编码器(编译存在 + 本机实测可初始化)。"""
    try:
        out = subprocess.run(
            [ffmpeg, "-hide_banner", "-encoders"],
            capture_output=True,
            text=True,
            timeout=15,
        ).stdout
    except Exception:
        return None

    for enc in _HW_ENCODERS:
        if enc not in out:
            continue
        spec = build_encoder_command("fast", enc)
        if verify_encoder(ffmpeg, spec.args):
            return enc
    return None


def verify_encoder(
    ffmpeg: str,
    enc_args: list[str],
    size: tuple[int, int] = (64, 64),
    nframes: int = 4,
    timeout: int = 20,
) -> bool:
    """用合成黑帧实测编码, 确认编码器真正可用(而非仅编译时列出)。"""
    w, h = size
    frame = b"\x00\x00\x00\xff" * (w * h)
    cmd = [
        ffmpeg, "-y", "-hide_banner", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", "bgra", "-s", f"{w}x{h}",
        "-r", str(nframes), "-i", "-",
        *enc_args, "-an", "-pix_fmt", "yuv420p",
        "NUL" if os.name == "nt" else "/dev/null",
    ]
    try:
        r = subprocess.run(cmd, input=frame * nframes,
                           capture_output=True, timeout=timeout)
        return r.returncode == 0
    except Exception:
        return False


def build_encoder_command(quality: str, hw_encoder: str | None) -> EncSpec:
    """按质量档位构建编码参数。"""
    quality = quality if quality in ("fast", "medium", "high") else "high"

    if hw_encoder == "h264_nvenc":
        return EncSpec(
            "NVENC (NVIDIA GPU)",
            ["-c:v", "h264_nvenc", "-preset", _NVENC_PRESET[quality],
             "-cq", str(_NVENC_CQ[quality]), "-rc", "vbr"],
        )
    if hw_encoder == "h264_qsv":
        return EncSpec(
            "QSV (Intel GPU)",
            ["-c:v", "h264_qsv", "-preset", "medium", "-global_quality",
             str(_NVENC_CQ[quality] + 2), "-look_ahead", "0"],
        )
    if hw_encoder == "h264_amf":
        return EncSpec(
            "AMF (AMD GPU)",
            ["-c:v", "h264_amf", "-quality", "speed", "-qp_i", str(_NVENC_CQ[quality]),
             "-qp_p", str(_NVENC_CQ[quality])],
        )
    if hw_encoder == "h264_videotoolbox":
        return EncSpec(
            "VideoToolbox (Apple)",
            ["-c:v", "h264_videotoolbox", "-quality", "speed"],
        )
    # 回退: 软件编码(尽可能快)
    return EncSpec(
        "libx264 (CPU, 兜底)",
        ["-c:v", "libx264", "-preset", _X264_PRESET[quality], "-crf",
         str(_X264_CRF[quality])],
    )


def build_command(
    ffmpeg: str,
    width: int,
    height: int,
    fps: int,
    enc: EncSpec,
    output: str,
) -> list[str]:
    """构建完整 ffmpeg 命令: rawvideo(BGRA) 管道输入 -> h264 mp4。"""
    return [
        ffmpeg,
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-f", "rawvideo",
        "-pix_fmt", "bgra",
        "-s", f"{width}x{height}",
        "-r", str(fps),
        "-i", "-",
        *enc.args,
        "-an",
        "-pix_fmt", "yuv420p",
        "-movflags", "+faststart",
        output,
    ]
