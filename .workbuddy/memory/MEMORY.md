# ScreenLive 项目长期笔记

## 项目约定
- 依赖装在项目内 `.venv`（E 盘），GUI 用 `.venv\Scripts\pythonw.exe main.py --gui` 启动（无控制台）
- venv 的 pythonw 启动时会派生子进程：Start-Process -PassThru 拿到的 PID 是父 stub，窗口属于子进程；验证用 `EnumWindows` 按 PID 过滤
- Git Bash 里 `tasklist //FI` 过滤参数会被误转义，直接 `tasklist | grep pythonw` 更可靠
- 测量窗口尺寸的脚本必须先 `SetProcessDpiAwareness(1)`，否则 GetWindowRect/GetSystemMetrics 返回虚拟化坐标（250% DPI 下 ÷2.5）
- 屏幕是 4K 3840×2160 @ 250% DPI（物理像素）；DPI 用 System 级（Per-Monitor 会让 Tk geometry 被 96/240 折算）

## 预览窗口（无边框方案，2026-08 定型）
- `overrideredirect(True)` 无边框 Toplevel：没有标题栏 → 不受系统最小跟踪宽度限制（高 DPI 下带标题栏窗口最小 328px），默认高度 = 屏幕高 1/8
- 移动/等比缩放全部用 Tk 鼠标事件自实现（`_pv_zone/_pv_press/_pv_drag/_pv_release`），拖身体移动、拖 14px 边缘/角落等比缩放，等比以输出像素为准，锚定固定边
- **比例感知最小尺寸**：竖比例 `min_w=120, min_h=max(100, round(120*rh/rw))`，次维度到界时反推主维度，钳制后比例不破
- 窗口重建（关闭预览再开）时几何按 config 恢复；无边框无标题栏，尺寸/比例信息画在画布左上角

## WDA_EXCLUDEFROMCAPTURE 关键坑（2026-08-09 发现）
- **窗口隐藏（withdraw）再显示（deiconify）后，DWM 排除面失效**：`GetWindowDisplayAffinity` 仍返回 17，但 BitBlt 捕获里该窗口区域变成黑块；预览与捕获区重叠时表现为"黑框"
- **重复 SetWindowDisplayAffinity(hwnd, 17)（值不变）是 no-op，无法重建排除面**；必须先置 0 再置 17（状态改变）才会强制重建 —— 等效用户手动"取消再勾选"
- 修复：`_toggle_preview` 显示分支 `after(150/350, _refresh_exclusion)`；`_start` 录制前也刷新
- 隔离测试（静态纯色窗口）复现不了该 bug，必须带实时渲染 + 鼠标追踪捕获的完整流程才能复现

## 编码/录制
- 时长准确：`_encode_loop` 节奏时钟与 `start_time` 锚定到第一帧实际到达时刻
- F9 热键必须在 `recorder.start()` 之前安装（pynput 首次导入约 1~2s，会争抢 GIL 延迟首帧）
