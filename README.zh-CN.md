# ScreenLive 屏幕录制工具

跟随鼠标的屏幕录制工具：捕获窗口始终以鼠标为中心，支持竖屏/横屏任意比例、动态视野缩放、鼠标操作可视化（点击特效/拖尾）、系统声音与麦克风录音。

## ✨ 特性

- **跟随鼠标录制**：捕获窗口始终居中于鼠标，光标移动时画面平滑跟随（可调跟随速度）
- **动态视野缩放**：鼠标大幅快速滑动时自动放大捕获区域（收进更多画面），停下来平滑恢复；可调灵敏度（低/中/高）
- **输入/输出分辨率分离**：捕获像素与视频像素独立设置——输入小输出大用**像素复制放大**，输入大输出小用**像素抽样缩小**（最近邻）
- **鼠标操作可视化**：点击特效（左键红/右键蓝/中键黄圆环，滚轮绿色方向指示）+ 鼠标拖尾轨迹
- **录音**：麦克风 / 系统声音（需启用"立体声混音"）直接混入视频，输出单文件 mp4（AAC 音轨）
- **任意比例**：9:16 / 16:9 / 1:1 / 3:4 / 4:3 / 9:21 / 自定义像素，固定比例下宽高自动联动
- **实时预览**：无边框预览窗口，可拖拽移动/等比缩放，置顶防遮挡，红色细线边界标示录制区域
- **排除本窗口**：录制画面不包含本软件窗口
- **硬件加速**：NVIDIA NVENC / Intel QSV / AMD AMF 硬件编码（可用时自动启用）

## 🖥 系统要求

- Windows 10 / 11（x64）
- 无需安装 Python 或任何依赖

## ⬇️ 下载与安装

从 [Releases 页面](https://github.com/HookyMaster/screenlive/releases/latest) 下载最新版本：

- `ScreenLive-vX.X.X-win64.zip` —— 完整可运行包（含运行库与 ffmpeg）

**安装**：解压 zip → 双击运行 `ScreenLive.exe`，无需安装。

## 🎬 使用说明

1. 启动后设置窗口出现，预览窗口自动打开（红色边界 = 录制区域）
2. 在设置窗口选择比例/像素、跟随速度、是否录音等
3. 按 **F9** 或点击"开始录制"开始；再按 **F9** 停止
4. 视频默认保存到 `Videos\ScreenLive`

**快捷键**：`F9` 开始/停止录制

**常见问题**：
- 录制电脑内部声音（游戏/视频声音）：在 声音设置 → 录制 中启用"立体声混音（Stereo Mix）"，重启软件后即可选择
- 录制中不能修改像素/比例（ffmpeg 帧尺寸恒定限制），请停止后修改

## 🔨 从源码构建

```bash
# 环境: Python 3.13, 依赖在项目内 .venv
.venv\Scripts\pip install -r requirements.txt

# 打包(完整命令含图标与 ffmpeg 捆绑):
.venv\Scripts\pyinstaller --noconfirm --windowed --name ScreenLive ^
  --icon icon.ico --add-data "icon.ico;." --add-data "icon256.png;." ^
  --collect-all imageio_ffmpeg --collect-all pynput ^
  --add-binary ".venv\Lib\site-packages\imageio_ffmpeg\binaries\ffmpeg-win-x86_64-v7.1.exe;." ^
  app.py
```

产物位于 `dist\ScreenLive\`（`ScreenLive.exe` + `_internal\` 运行库，整个目录即完整可运行包）。

## 📖 关于

- **技术栈**：Python 3.13 · Tkinter · numpy · mss（屏幕捕获）· ffmpeg（编码/音频）· pynput（全局鼠标监听）
- **功能定位**：为"操作演示/教程录制"设计——鼠标轨迹、点击位置、跟随视野都看得清
- **许可证**：MIT

---

[English README](README.md)
