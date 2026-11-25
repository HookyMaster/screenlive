#![windows_subsystem = "windows"]

use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use tauri::{LogicalSize, Manager, PhysicalPosition, State, WebviewWindow};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND, LPARAM},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
            SIGDN_FILESYSPATH,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowTextW,
            IsWindowVisible, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
            WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        },
    },
    core::BOOL,
};

#[derive(Serialize)]
struct Source {
    id: String,
    title: String,
}

#[tauri::command]
fn list_sources(window: WebviewWindow) -> Result<Vec<Source>, String> {
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let sources = unsafe { &mut *(data.0 as *mut Vec<Source>) };
        let mut text = [0u16; 512];
        let eligible = unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut u32).cast(),
                size_of::<u32>() as u32,
            );
            IsWindowVisible(hwnd).as_bool()
                && cloaked == 0
                && (style & WS_EX_APPWINDOW.0 != 0
                    || (style & WS_EX_TOOLWINDOW.0 == 0 && GetWindow(hwnd, GW_OWNER).is_err()))
        };
        if eligible {
            let count = unsafe { GetWindowTextW(hwnd, &mut text) };
            if count > 0 {
                let title = String::from_utf16_lossy(&text[..count as usize])
                    .trim()
                    .to_owned();
                if !title.is_empty() {
                    sources.push(Source {
                        id: format!("{:p}", hwnd.0),
                        title,
                    });
                }
            }
        }
        BOOL(1)
    }
    let mut sources: Vec<Source> = Vec::new();
    unsafe {
        EnumWindows(
            Some(visit),
            LPARAM((&mut sources as *mut Vec<Source>) as isize),
        )
    }
    .map_err(message)?;
    if let Ok(hwnd) = window.hwnd() {
        let own = format!("{:p}", hwnd.0);
        sources.retain(|source| source.id != own);
    }
    let mut seen = std::collections::HashSet::new();
    sources.retain(|source| seen.insert(source.id.clone()));
    sources.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)));
    let mut remaining = sources.as_mut_slice();
    while let Some(first) = remaining.first() {
        let count = remaining.partition_point(|source| source.title == first.title);
        let (same_title, rest) = remaining.split_at_mut(count);
        if count > 1 {
            for (index, source) in same_title.iter_mut().enumerate() {
                source.title.push_str(&format!(" · 窗口 {}", index + 1));
            }
        }
        remaining = rest;
    }
    sources.insert(
        0,
        Source {
            id: "primary-display".into(),
            title: "整个主显示器".into(),
        },
    );
    Ok(sources)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    source: String,
    ratio: String,
    mode: String,
    background: String,
    preview: bool,
    output_device: Option<String>,
    input_device: Option<String>,
    output_folder: Option<String>,
    close_range: Option<u32>,
    #[serde(default)]
    cursor_trail: bool,
    cursor_color: Option<String>,
    #[serde(default)]
    include_panel: bool,
}

fn prepare_sources(window: &WebviewWindow, options: &Options) -> Result<(), String> {
    let selection = &options.source;
    let available = list_sources(window.clone())?;
    let ids: Vec<_> = selection.split(',').collect();
    let unique: std::collections::HashSet<_> = ids.iter().copied().collect();
    let regions = ids
        .iter()
        .filter(|id| screenlive_windows::parse_region(id).is_some())
        .count();
    if ids.len() > 8
        || regions > 1
        || unique.len() != ids.len()
        || ids.iter().any(|id| {
            screenlive_windows::parse_region(id).is_none()
                && !available.iter().any(|source| source.id == *id)
        })
        || (ids.len() > 1 && unique.contains("primary-display"))
    {
        return Err("采集来源不可用。最多选择 8 个窗口，整屏需单独选择。".into());
    }
    let hwnd = window.hwnd().map_err(message)?;
    let affinity = if options.include_panel {
        WDA_NONE
    } else {
        WDA_EXCLUDEFROMCAPTURE
    };
    unsafe { SetWindowDisplayAffinity(HWND(hwnd.0), affinity) }.map_err(message)?;
    Ok(())
}

fn camera_args(command: &mut Command, options: &Options) {
    command.args([
        "--close-range",
        &options.close_range.unwrap_or(70).to_string(),
    ]);
    if options.cursor_trail {
        command.arg("--cursor-trail");
    }
    if let Some(color) = &options.cursor_color {
        command.args(["--cursor-color", color]);
    }
}

fn preview_args(command: &mut Command, window: &WebviewWindow, include_panel: bool) -> Result<(), String> {
    command.arg("--preview-size-file").arg(config_path(window)?.with_file_name("preview-size"));
    if let Ok(hwnd) = window.hwnd() {
        let id = format!("{:p}", hwnd.0);
        command.args(["--preview-near", &id]);
        if include_panel {
            command.arg("--include-panel");
        }
    }
    Ok(())
}

#[tauri::command]
async fn pick_region(
    window: WebviewWindow,
    preview: State<'_, LivePreview>,
) -> Result<Option<String>, String> {
    let owner = format!("{:p}", window.hwnd().map_err(message)?.0);
    let exe = binary("record.exe")?;
    stop_preview(&preview)?;
    window.hide().map_err(message)?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        Command::new(exe)
            .args(["--pick-region", &owner])
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .output()
            .map_err(message)
    })
    .await;
    let _ = window.show();
    let _ = window.set_focus();
    let result = result.map_err(message)??;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().into());
    }
    let result = String::from_utf8_lossy(&result.stdout).trim().to_owned();
    if result.is_empty() {
        return Ok(None);
    }
    if screenlive_windows::parse_region(&result).is_none() {
        return Err("框选区域无效".into());
    }
    Ok(Some(result))
}

#[tauri::command]
async fn pick_folder(window: WebviewWindow) -> Result<Option<String>, String> {
    let owner = window.hwnd().map_err(message)?.0 as isize;
    tauri::async_runtime::spawn_blocking(move || {
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(message)?;
            let _apartment = Apartment;
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(message)?;
            dialog
                .SetOptions(
                    dialog.GetOptions().map_err(message)? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM,
                )
                .map_err(message)?;
            match dialog.Show(Some(HWND(owner as *mut _))) {
                Ok(()) => {}
                Err(error)
                    if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(message(error)),
            }
            let item = dialog.GetResult().map_err(message)?;
            let raw = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(message)?;
            let result = raw.to_string();
            CoTaskMemFree(Some(raw.0.cast()));
            result.map(Some).map_err(message)
        }
    })
    .await
    .map_err(message)?
}

struct Recording {
    child: Child,
    path: PathBuf,
    saving: Arc<AtomicU8>,
}
type Session = Mutex<Option<Recording>>;

fn config_path(window: &WebviewWindow) -> Result<PathBuf, String> {
    Ok(window
        .app_handle()
        .path()
        .app_config_dir()
        .map_err(message)?
        .join("config.json"))
}

#[tauri::command]
fn load_config(window: WebviewWindow) -> Result<serde_json::Value, String> {
    let file = config_path(&window)?;
    match std::fs::read(file) {
        Ok(bytes) if bytes.len() <= 32768 => serde_json::from_slice(&bytes).map_err(message),
        Ok(_) => Err("配置文件超出大小限制。".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(error) => Err(message(error)),
    }
}

#[tauri::command]
fn save_config(window: WebviewWindow, config: serde_json::Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&config).map_err(message)?;
    if !config.is_object() || bytes.len() > 32768 {
        return Err("配置内容无效。".into());
    }
    let file = config_path(&window)?;
    std::fs::create_dir_all(file.parent().unwrap()).map_err(message)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(message)?
        .as_nanos();
    let temporary = file.with_extension(format!("{}.{}.tmp", std::process::id(), stamp));
    std::fs::write(&temporary, bytes).map_err(message)?;
    std::fs::rename(temporary, file).map_err(message)
}

#[derive(Default)]
struct AudioMeters(Mutex<[Option<(String, screenlive_windows::audio::Meter)>; 2]>);

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Level {
    Off,
    Peak { db: f32 },
    Failed { error: String },
}

#[tauri::command]
fn read_levels(
    session: State<Session>,
    meters: State<AudioMeters>,
    input: Option<String>,
    output: Option<String>,
) -> Result<[Level; 2], String> {
    let recording = session.lock().map_err(message)?;
    let mut active = meters.0.lock().map_err(message)?;
    if recording.is_some() {
        *active = [None, None];
        return Ok([Level::Off, Level::Off]);
    }
    let mut levels = [Level::Off, Level::Off];
    for (index, id) in [input, output].into_iter().enumerate() {
        let slot = &mut active[index];
        if slot.as_ref().map(|(id, _)| id) != id.as_ref() {
            *slot = None;
            if let Some(id) = id {
                let meter = screenlive_windows::audio::Meter::start(id.clone(), index == 1);
                *slot = Some((id, meter));
            }
        }
        if let Some((_, meter)) = slot {
            levels[index] = match meter.read() {
                Ok(db) => Level::Peak { db },
                Err(error) => Level::Failed { error },
            };
        }
    }
    Ok(levels)
}

#[derive(Default)]
struct LivePreview(Mutex<Option<Child>>);

fn stop_preview(preview: &LivePreview) -> Result<(), String> {
    if let Some(mut child) = preview.0.lock().map_err(message)?.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

#[tauri::command]
fn set_preview(
    window: WebviewWindow,
    session: State<Session>,
    preview: State<LivePreview>,
    options: Options,
) -> Result<(), String> {
    let recording = session.lock().map_err(message)?;
    if recording.is_some() {
        return Ok(());
    }
    stop_preview(&preview)?;
    if !options.preview || options.source.is_empty() {
        return Ok(());
    }
    prepare_sources(&window, &options)?;
    let size = match options.ratio.as_str() {
        "9:16" => "360x640",
        "16:9" => "640x360",
        "1:1" => "480x480",
        "4:5" => "384x480",
        "4:3" => "640x480",
        "3:4" => "360x480",
        _ => return Err("不支持该画幅".into()),
    };
    let mut command = Command::new(binary("record.exe")?);
    command
        .arg(&options.source)
        .arg(binary("ffmpeg.exe")?)
        .args([
            "-",
            "3600",
            "--controlled",
            "--preview-only",
            "--size",
            size,
        ])
        .creation_flags(0x08000000)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if options.mode == "fit" {
        command.arg("--fit");
    }
    command.args(["--background", &options.background]);
    camera_args(&mut command, &options);
    preview_args(&mut command, &window, options.include_panel)?;
    *preview.0.lock().map_err(message)? = Some(command.spawn().map_err(message)?);
    Ok(())
}

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn binary(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(message)?;
    if name == "record.exe" {
        let latest = exe.with_file_name("record-backgrounds.exe");
        if latest.is_file() {
            return Ok(latest);
        }
    }
    let sibling = exe.with_file_name(name);
    if sibling.is_file() {
        return Ok(sibling);
    }
    Err(format!("找不到随应用提供的 {name}"))
}

#[derive(Serialize)]
struct WallpaperChoice {
    id: &'static str,
    title: &'static str,
    thumbnail: String,
}

#[tauri::command]
async fn background_choices() -> Result<Vec<WallpaperChoice>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let ffmpeg = binary("ffmpeg.exe")?;
        let mut choices = Vec::new();
        for (id, title, _) in screenlive_windows::background::WALLPAPERS {
            let Some(path) = screenlive_windows::background::wallpaper(id) else {
                continue;
            };
            let image = Command::new(&ffmpeg)
                .creation_flags(0x08000000)
                .args(["-hide_banner", "-loglevel", "error", "-i"])
                .arg(&path)
                .args([
                    "-vf",
                    "scale=96:54:force_original_aspect_ratio=increase,crop=96:54",
                    "-frames:v",
                    "1",
                    "-c:v",
                    "mjpeg",
                    "-f",
                    "image2pipe",
                    "pipe:1",
                ])
                .output()
                .map_err(message)?;
            if image.status.success() && !image.stdout.is_empty() {
                choices.push(WallpaperChoice {
                    id,
                    title,
                    thumbnail: format!("data:image/jpeg;base64,{}", STANDARD.encode(image.stdout)),
                });
            }
        }
        Ok(choices)
    })
    .await
    .map_err(message)?
}

#[derive(Serialize)]
struct Devices {
    input: Vec<(String, String)>,
    output: Vec<(String, String)>,
}

#[tauri::command]
async fn audio_devices() -> Result<Devices, String> {
    tauri::async_runtime::spawn_blocking(|| {
        Ok(Devices {
            input: screenlive_windows::audio::endpoints(false).map_err(message)?,
            output: screenlive_windows::audio::endpoints(true).map_err(message)?,
        })
    })
    .await
    .map_err(message)?
}

#[tauri::command]
fn begin(
    window: WebviewWindow,
    session: State<Session>,
    preview: State<LivePreview>,
    meters: State<AudioMeters>,
    options: Options,
) -> Result<(), String> {
    let mut session = session.lock().map_err(message)?;
    if session.is_some() {
        return Err("已有录制任务".into());
    }
    *meters.0.lock().map_err(message)? = [None, None];
    prepare_sources(&window, &options)?;
    let size = match options.ratio.as_str() {
        "9:16" => "1080x1920",
        "16:9" => "1920x1080",
        "1:1" => "1080x1080",
        "4:5" => "1080x1350",
        "4:3" => "1440x1080",
        "3:4" => "1080x1440",
        _ => return Err("不支持该画幅".into()),
    };
    let dir = if let Some(ref folder) = options.output_folder {
        let folder = PathBuf::from(folder);
        if !folder.is_absolute() || !folder.is_dir() {
            return Err("保存目录不可用，请在「更多」里重新选择。".into());
        }
        folder
    } else {
        let default = window
            .app_handle()
            .path()
            .video_dir()
            .map_err(message)?
            .join("ScreenLive");
        std::fs::create_dir_all(&default).map_err(message)?;
        default
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(message)?
        .as_millis();
    let path = dir.join(format!("ScreenLive-{timestamp}.mp4"));
    let mut cmd = Command::new(binary("record.exe")?);
    cmd.arg(&options.source)
        .arg(binary("ffmpeg.exe")?)
        .arg(&path)
        .args(["3600", "--controlled", "--size", size])
        .creation_flags(0x08000000)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if options.mode == "fit" {
        cmd.arg("--fit");
    }
    if options.preview {
        cmd.arg("--preview");
    }
    cmd.args(["--background", &options.background]);
    camera_args(&mut cmd, &options);
    preview_args(&mut cmd, &window, options.include_panel)?;
    if let Some(name) = options.input_device {
        cmd.args(["--mic", &name]);
    }
    if let Some(name) = options.output_device {
        cmd.args(["--loopback", &name]);
    }
    stop_preview(&preview)?;
    let mut child = cmd.spawn().map_err(message)?;
    let saving = Arc::new(AtomicU8::new(0));
    if let Some(stdout) = child.stdout.take() {
        let progress = saving.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(value) = line
                    .strip_prefix("SAVE_PROGRESS ")
                    .and_then(|p| p.parse::<u8>().ok())
                {
                    progress.fetch_max(value.min(100), Ordering::Relaxed);
                }
            }
        });
    }
    *session = Some(Recording {
        child,
        path,
        saving,
    });
    Ok(())
}

#[tauri::command]
fn control(session: State<Session>, action: String) -> Result<(), String> {
    if !matches!(action.as_str(), "stop" | "preview") {
        return Err("未知录制操作".into());
    }
    let mut session = session.lock().map_err(message)?;
    if let Some(recording) = session.as_mut() {
        if let Some(input) = recording.child.stdin.as_mut() {
            writeln!(input, "{action}").map_err(message)?;
        }
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Status {
    Running,
    Saving { progress: u8 },
    Saved { path: String },
    Failed { error: String },
}

#[tauri::command]
fn recording_status(window: WebviewWindow, session: State<Session>) -> Result<Status, String> {
    let mut session = session.lock().map_err(message)?;
    let Some(recording) = session.as_mut() else {
        return Err("没有录制任务".into());
    };
    let Some(status) = recording.child.try_wait().map_err(message)? else {
        let progress = recording.saving.load(Ordering::Relaxed);
        return Ok(if progress > 0 {
            Status::Saving { progress }
        } else {
            Status::Running
        });
    };
    let recording = session.take().unwrap();
    let _ = window.show();
    let _ = window.set_focus();
    if status.success() && recording.path.is_file() {
        return Ok(Status::Saved {
            path: recording.path.to_string_lossy().into_owned(),
        });
    }
    let output = recording.child.wait_with_output().map_err(message)?;
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Ok(Status::Failed {
        error: if error.is_empty() {
            format!("录制退出：{status}")
        } else {
            error
        },
    })
}

#[tauri::command]
fn show_recording(path: String, folder: bool) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("mp4") {
        return Err("成片不可用".into());
    }
    let mut cmd = Command::new("explorer.exe");
    if folder {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    cmd.spawn().map_err(message)?;
    Ok(())
}

#[tauri::command]
fn size_panel(window: WebviewWindow, collapsed: bool, menu: Option<bool>) -> Result<(), String> {
    let (width, height) = if collapsed && menu == Some(true) {
        (188.0, 82.0)
    } else if collapsed {
        (66.0, 66.0)
    } else {
        (286.0, 330.0)
    };
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(message)
}

#[tauri::command]
fn drag_panel(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(message)
}

#[tauri::command]
fn panel_ready(window: WebviewWindow) -> Result<(), String> {
    // Center the dial itself; the hint below it takes up the rest of the window.
    if window.center().is_ok() {
        if let (Ok(position), Ok(scale)) = (window.outer_position(), window.scale_factor()) {
            let _ = window.set_position(PhysicalPosition::new(
                position.x,
                position.y + (24.0 * scale).round() as i32,
            ));
        }
    }
    window.show().map_err(message)
}

#[tauri::command]
fn exit_app(
    window: WebviewWindow,
    session: State<Session>,
    preview: State<LivePreview>,
) -> Result<(), String> {
    if session.lock().map_err(message)?.is_some() {
        return Err("请先停止录制并等待保存完成。".into());
    }
    stop_preview(&preview)?;
    window.app_handle().exit(0);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(Session::default())
        .manage(LivePreview::default())
        .manage(AudioMeters::default())
        .invoke_handler(tauri::generate_handler![
            list_sources,
            audio_devices,
            begin,
            control,
            recording_status,
            show_recording,
            size_panel,
            drag_panel,
            set_preview,
            exit_app,
            pick_folder,
            panel_ready,
            read_levels,
            background_choices,
            load_config,
            save_config,
            pick_region
        ])
        .run(tauri::generate_context!())
        .expect("ScreenLive desktop startup failed");
}
