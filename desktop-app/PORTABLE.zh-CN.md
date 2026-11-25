# ScreenLive Windows 便携版

从 [FFmpeg Windows 构建](https://www.gyan.dev/ffmpeg/builds/)下载 Essentials ZIP，将其中的 `bin/ffmpeg.exe` 放到本文件夹，再双击 `ScreenLive.exe`。请将 `record-backgrounds.exe` 和 `WebView2Loader.dll` 保留在同一文件夹。运行需要 Windows Graphics Capture 和 Microsoft Edge WebView2 Runtime。

1. 在「来源」选择窗口、整个主显示器或「框选区域」。同时选择窗口与框选区域时，只采集已选窗口落在框内的部分。框选时按 Esc 或右键可以取消。
2. 在「预览」显示或隐藏实时画面、调整最小取景范围。可拖动预览窗口，拉伸边缘会等比缩放并自动保存大小。
3. 按需选择画幅、构图、背景和声音设备。默认输出为 1080×1920、30 fps。
4. 点击圆盘中央开始录制，圆盘会收成宝石。点开宝石后可结束录制，也可按 F9。F8 可隐藏或恢复预览。
5. 「更多」可设置保存路径、鼠标拖尾及是否在预览和录制中显示本软件。默认保存到「视频/ScreenLive」，保存进度显示在圆盘外沿。

「声音」面板会显示已启用设备的实时电平。电脑声音通过选定的播放设备录制，麦克风需要允许 Windows 桌面应用访问。

设置保存在 Windows 应用数据目录中的 `app.screenlive.desktop`。重新启动后，采集窗口需要重新确认。
