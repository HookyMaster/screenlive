# 第三方组件

- Microsoft Edge WebView2 加载器随便携包提供，按 [Microsoft WebView2 分发条款](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution)使用。
- FFmpeg 作为独立进程运行，由用户从其 Windows 构建发布方单独下载。Gyan Essentials Windows 构建采用 GPLv3 或后续版本，见 [构建和源码入口](https://www.gyan.dev/ffmpeg/builds/)及 [FFmpeg 许可证说明](https://ffmpeg.org/legal.html)。
- Rust 依赖及版本列于各模块的 `Cargo.lock`，各依赖适用其上游声明的许可证，见 [Rust 依赖许可声明](docs/RUST-DEPENDENCY-LICENSES.txt)。ScreenLive 自身的非商业许可适用于本项目的自有内容。
