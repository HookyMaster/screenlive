# ScreenLive — Focus-Follow Screen Recorder

A screen recorder that keeps the capture window centered on your mouse. Supports portrait/landscape aspect ratios, dynamic field-of-view zoom, mouse action visualization (click effects / trail), and system/microphone audio recording.

## ✨ Features

- **Focus-follow recording**: capture window always centers on the cursor with smooth tracking (adjustable follow speed)
- **Dynamic FOV zoom**: fast, large mouse swipes automatically widen the capture area (see more of the screen), smoothly returning to normal when you slow down; sensitivity adjustable (Low / Medium / High)
- **Separate input/output resolution**: capture pixels and video pixels are independent — small input → large output uses **nearest-neighbor upscaling (pixel copy)**, large input → small output **downsamples by pixel sampling**
- **Mouse action visualization**: click effects (left = red, right = blue, middle = yellow ring; scroll = green direction indicator) + mouse trail
- **Audio recording**: microphone / system audio (requires "Stereo Mix") mixed directly into the video — single-file mp4 with AAC track
- **Any aspect ratio**: 9:16, 16:9, 1:1, 3:4, 4:3, 9:21 or fully custom pixels; width/height auto-linked for fixed ratios
- **Live preview**: borderless preview window — drag to move, edge-drag to resize proportionally, always-on-top, with a red border marking the capture area
- **Self-exclusion**: the app's own windows are excluded from recording
- **Hardware acceleration**: NVIDIA NVENC / Intel QSV / AMD AMF used automatically when available

## 🖥 System Requirements

- Windows 10 / 11 (x64)
- No Python or any runtime dependency required

## ⬇️ Download & Install

Get the latest build from the [Releases page](https://github.com/HookyMaster/screenlive/releases/latest):

- `ScreenLive-vX.X.X-win64.zip` — fully self-contained package (runtime + bundled ffmpeg)

**Install**: unzip → double-click `ScreenLive.exe`. No installation needed.

## 🎬 Usage

1. Launch — the settings window appears and the preview opens (red border = capture area)
2. Choose aspect ratio / pixels, follow speed, audio, etc. in the settings window
3. Press **F9** or click "Start Recording" to begin; press **F9** again to stop
4. Videos are saved to `Videos\ScreenLive` by default

**Hotkey**: `F9` start/stop recording

**Troubleshooting**:
- To record internal audio (game/video sound): enable "Stereo Mix" in Sound settings → Recording, then restart the app to select it
- Resolution/ratio cannot be changed while recording (ffmpeg requires constant frame size) — stop first, then change

## 🔨 Build from Source

```bash
# Env: Python 3.13, dependencies in project-local .venv
.venv\Scripts\pip install -r requirements.txt

# Package (full command with icon & bundled ffmpeg):
.venv\Scripts\pyinstaller --noconfirm --windowed --name ScreenLive ^
  --icon icon.ico --add-data "icon.ico;." --add-data "icon256.png;." ^
  --collect-all imageio_ffmpeg --collect-all pynput ^
  --add-binary ".venv\Lib\site-packages\imageio_ffmpeg\binaries\ffmpeg-win-x86_64-v7.1.exe;." ^
  app.py
```

Output is in `dist\ScreenLive\` — the whole directory (`ScreenLive.exe` + `_internal\`) is the self-contained package.

## 📖 About

- **Tech stack**: Python 3.13 · Tkinter · numpy · mss (screen capture) · ffmpeg (encode/audio) · pynput (global mouse hook)
- **Purpose**: built for demo/tutorial recording — mouse trail, click positions and focus are all clearly visible
- **License**: MIT

---

[中文说明](README.zh-CN.md)
