# ScreenLive for macOS 0.3.0

适用于 macOS 15 及以上版本。通过 macOS 系统选择器选择显示器或窗口，可进一步框选区域。支持实时预览、鼠标跟随、鼠标拖尾、系统音频和麦克风。

MP4 以 30 fps 录制。16:9 的输出尺寸为 3840 × 2160，9:16 为 2160 × 3840，其他预设画幅的短边为 2160 像素。最终细节取决于来源的实际像素和取景范围。

## 构建

在装有 macOS 15 或更新版 SDK 的 Mac 上，从仓库根目录运行：

```sh
bash platform/macos/build.sh arm64
```

Intel 目标使用 `x86_64`。生成的应用位于 `platform/macos/dist/ScreenLive.app`，默认使用 ad-hoc 签名。
