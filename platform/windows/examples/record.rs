//! End-to-end Windows recorder. Uses a native preview and an external FFmpeg executable.
#[path = "support/region_picker.rs"]
mod region_picker;
use std::{
    cell::Cell,
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use screenlive_core::{Blit, Director, Mode, Point, Rect, Size};
use screenlive_windows::{Poll, Readback, WindowCapture, parse_region};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetMonitorInfoW, HALFTONE,
            HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
            MonitorFromRect, MonitorFromWindow, ReleaseDC, SRCCOPY, SetBrushOrgEx,
            SetStretchBltMode, StretchDIBits,
        },
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F8, VK_F9, VK_LBUTTON},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GW_HWNDNEXT,
                GWLP_WNDPROC, GetClientRect, GetCursorPos, GetTopWindow, GetWindow, GetWindowRect,
                IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IsIconic, IsWindow,
                IsWindowVisible, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, SW_HIDE, SW_SHOWNA,
                SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetCursor, SetProcessDPIAware,
                SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow,
                TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WM_LBUTTONDOWN, WM_SETCURSOR,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::w,
};

const OUTPUT: Size = Size {
    width: 720,
    height: 1280,
};
const FPS: u32 = 30;
type AnyError = Box<dyn std::error::Error>;

fn main() -> Result<(), AnyError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--pick-region") {
        let id = args.get(1).ok_or("Missing panel window")?;
        let hwnd = HWND(usize::from_str_radix(id.trim_start_matches("0x"), 16)? as *mut _);
        unsafe {
            let _ = SetProcessDPIAware();
        }
        if let Some(r) = region_picker::select(hwnd)? {
            println!("region:{}:{}:{}:{}", r.left, r.top, r.right, r.bottom);
        }
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--audio-devices") {
        for (label, loopback) in [("Input", false), ("Output", true)] {
            for (_, name) in screenlive_windows::audio::endpoints(loopback)? {
                println!("{label}: {name}");
            }
        }
        return Ok(());
    }
    if args.len() < 4 {
        return Err("Usage: record HWND_HEX FFMPEG_EXE OUTPUT.mp4 SECONDS [--preview] [--fit] [--mic DEVICE] [--loopback DEVICE]".into());
    }
    let fullscreen = args[0] == "primary-display";
    let handles = if fullscreen {
        vec![(HWND::default(), None)]
    } else {
        args[0]
            .split(',')
            .map(|id| {
                if id.starts_with("region:") {
                    return Ok((
                        HWND::default(),
                        Some(parse_region(id).ok_or("Invalid capture region")?),
                    ));
                }
                usize::from_str_radix(id.trim_start_matches("0x"), 16)
                    .map(|value| (HWND(value as *mut _), None))
                    .map_err(|_| "Invalid window handle")
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if handles.is_empty() || handles.len() > 8 {
        return Err("Select between one and eight windows".into());
    }
    let seconds: u32 = args[3].parse()?;
    if seconds == 0 || seconds > 3600 {
        return Err("SECONDS must be between 1 and 3600".into());
    }
    let (mut preview, mut mode, mut audio) = (false, Mode::Follow, Vec::new());
    let (mut output, mut controlled, mut background) = (OUTPUT, false, "gradient");
    let mut preview_only = false;
    let mut preview_near = None;
    let mut include_panel = false;
    let mut close_range = 70;
    let mut cursor_trail = false;
    let mut cursor_color = [115, 135, 255];
    let mut preview_size_file = None;
    let mut flags = args[4..].iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--preview" => preview = true,
            "--include-panel" | "--demo-preview" => include_panel = true,
            "--cursor-trail" => cursor_trail = true,
            "--cursor-color" => {
                let hex = flags
                    .next()
                    .and_then(|value| value.strip_prefix('#'))
                    .filter(|value| value.len() == 6)
                    .ok_or("Expected a #RRGGBB cursor color")?;
                let rgb = u32::from_str_radix(hex, 16)?;
                cursor_color = [rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8];
            }
            "--preview-size-file" => {
                preview_size_file = Some(PathBuf::from(
                    flags.next().ok_or("Missing preview size file")?,
                ));
            }
            "--fit" => mode = Mode::Fit,
            "--controlled" => controlled = true,
            "--preview-only" => {
                preview_only = true;
                preview = true;
            }
            "--preview-near" => {
                let handle = flags.next().ok_or("Missing control-panel window handle")?;
                preview_near = Some(HWND(
                    usize::from_str_radix(handle.trim_start_matches("0x"), 16)? as *mut _,
                ));
            }
            "--solid" => background = "solid",
            "--background" => background = flags.next().ok_or("Missing background style")?,
            "--close-range" => {
                close_range = flags
                    .next()
                    .ok_or("Missing closest framing size")?
                    .parse()?
            }
            "--size" => {
                let (w, h) = flags
                    .next()
                    .and_then(|s| s.split_once('x'))
                    .ok_or("Expected WIDTHxHEIGHT")?;
                output = Size {
                    width: w.parse()?,
                    height: h.parse()?,
                };
                if [output.width, output.height]
                    .into_iter()
                    .any(|v| !(2..=3840).contains(&v) || v % 2 != 0)
                {
                    return Err("Output dimensions must be even and between 2 and 3840".into());
                }
            }
            "--mic" | "--loopback" => {
                let device = flags.next().ok_or("Missing Windows audio endpoint")?;
                if audio.len() >= 2 {
                    return Err("At most two audio inputs are supported".into());
                }
                audio.push((device.as_str(), flag == "--loopback"));
            }
            _ => return Err(format!("Unknown option: {flag}").into()),
        }
    }
    let background_size = if preview_only {
        preview_render_size(output, 1920)
    } else {
        output
    };
    let background_pixels = screenlive_windows::background::render(
        &args[1],
        background,
        background_size.width,
        background_size.height,
    )?;
    unsafe {
        let _ = SetProcessDPIAware();
    }
    let monitor =
        fullscreen.then(|| unsafe { MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY) });
    let region = handles.iter().find_map(|(_, rectangle)| *rectangle);
    let has_windows = handles.iter().any(|(hwnd, _)| !hwnd.is_invalid());
    let capture_display = fullscreen || (region.is_some() && !has_windows);
    let mut camera = Director::new(output, mode, close_range);
    let mut view = if preview || controlled {
        Some(Preview::new(
            output,
            preview,
            capture_display,
            preview_near,
            preview_size_file,
        )?)
    } else {
        None
    };
    let mut surfaces = handles
        .into_iter()
        .filter(|(hwnd, _)| !has_windows || !hwnd.is_invalid())
        .map(|(hwnd, _)| Surface::start(hwnd, monitor, region))
        .collect::<windows::core::Result<Vec<_>>>()?;
    let panel = preview_near.filter(|_| include_panel && !capture_display);
    if let Some(panel) = panel {
        if !surfaces.iter().any(|surface| surface.hwnd == panel) {
            surfaces.push(Surface::start(panel, None, region)?);
        }
    }
    let (commands, requested) = std::sync::mpsc::channel();
    if controlled {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if commands.send(line).is_err() {
                    return;
                }
            }
            let _ = commands.send("stop".into());
        });
    }
    let path = Path::new(&args[2]);
    let temp = path.with_extension("partial.mp4");
    if !preview_only && (path.exists() || temp.exists()) {
        return Err("Output or recovery file already exists".into());
    }
    let mut tracks = Vec::new();
    let media_started = Instant::now();
    let mut ffmpeg = Command::new(&args[1]);
    ffmpeg
        .creation_flags(0x08000000)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let clock_offset = if audio.is_empty() {
        None
    } else {
        let qpc_epoch = screenlive_windows::audio::clock()?;
        let epoch = SystemTime::now().duration_since(UNIX_EPOCH)?;
        for (index, (endpoint, loopback)) in audio.iter().enumerate() {
            let track_path = path.with_extension(format!("audio-{index}.partial.pcm"));
            tracks.push(screenlive_windows::audio::Track::start(
                (*endpoint).into(),
                *loopback,
                track_path,
                qpc_epoch,
            )?);
        }
        ffmpeg.arg("-copyts");
        Some(format!("-{}.{:06}", epoch.as_secs(), epoch.subsec_micros()))
    };
    if let Some(offset) = &clock_offset {
        timestamp_input(&mut ffmpeg, offset);
        ffmpeg.args([
            "-thread_queue_size",
            "8",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
        ]);
    }
    ffmpeg.args([
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-progress",
        "pipe:1",
        "-n",
        "-f",
        "rawvideo",
        "-pixel_format",
        "bgra",
        "-video_size",
        &format!("{}x{}", output.width, output.height),
        "-framerate",
        "30",
        "-i",
        "pipe:0",
    ]);
    ffmpeg.args(["-map", "0:v:0"]);
    if !audio.is_empty() {
        // Fill gaps on the shared clock without speeding up video to catch up.
        ffmpeg.args(["-vf", "fps=30:start_time=0"]);
    }
    let mut child = if preview_only {
        None
    } else {
        Some(
            ffmpeg
                .args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "superfast",
                    "-crf",
                    "16",
                    "-tune",
                    "zerolatency",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(&temp)
                .spawn()?,
        )
    };
    let mut input = child.as_mut().and_then(|child| child.stdin.take());
    let progress = child
        .as_mut()
        .and_then(|child| child.stdout.take())
        .map(|stdout| {
            std::thread::spawn(move || {
                BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                    .filter_map(|line| line.strip_prefix("out_time_us=")?.parse::<u64>().ok())
                    .last()
            })
        });
    if child.is_some() && input.is_none() {
        return Err("FFmpeg stdin is unavailable".into());
    }
    let result = (|| -> Result<u32, AnyError> {
        let mut composite = vec![0; output.width as usize * output.height as usize * 4];
        let mut render_size = output;
        let mut cached_background = if preview_only {
            resize_background(&background_pixels, background_size, output)
        } else {
            Vec::new()
        };
        let mut resize_candidate = (output, Instant::now());
        let mut layer_order = Vec::with_capacity(surfaces.len());
        let mut count = 0;
        let begin = Instant::now();
        let mut f8_down = false;
        let mut f9_down = key_down(VK_F9.0);
        let mut stop_requested = false;
        let rate = if preview_only { 15 } else { FPS };
        let mut last_camera_tick = Instant::now();
        let frame_period = 1.0 / f64::from(rate);
        let mut cursor_history = VecDeque::<(Instant, Point)>::new();
        'frames: for index in 0..seconds * rate {
            let deadline = begin + Duration::from_secs_f64(index as f64 * frame_period);
            sleep(deadline.saturating_duration_since(Instant::now()));
            for command in requested.try_iter() {
                if command == "stop" {
                    stop_requested = true;
                }
                if command == "preview" {
                    if let Some(view) = view.as_mut() {
                        view.toggle();
                    }
                }
            }
            let f9 = key_down(VK_F9.0);
            if index > 0 && (stop_requested || (!preview_only && f9 && !f9_down)) {
                break 'frames;
            }
            f9_down = f9;
            if preview_only {
                if let Some(view) = view.as_ref() {
                    if let Some(edge) = view.client_long_edge() {
                        let desired = preview_render_size(output, edge);
                        if desired != resize_candidate.0 {
                            resize_candidate = (desired, Instant::now());
                        } else if desired != render_size
                            && resize_candidate.1.elapsed() >= Duration::from_millis(120)
                        {
                            render_size = desired;
                            camera.set_output_size(render_size);
                            composite.resize(
                                render_size.width as usize * render_size.height as usize * 4,
                                0,
                            );
                            cached_background =
                                resize_background(&background_pixels, background_size, render_size);
                        }
                    }
                }
            }
            for surface in &mut surfaces {
                surface.poll()?;
            }
            let placeholder = if include_panel {
                view.as_ref()
                    .filter(|view| view.visible)
                    .and_then(|view| surface_bounds(view.hwnd, None))
            } else {
                None
            };
            let bounds = surfaces
                .iter()
                .filter_map(|s| s.bounds)
                .chain(placeholder)
                .reduce(join_rects);
            refresh_layer_order(&surfaces, &mut layer_order);
            let hovered = desktop_pointer().and_then(|p| {
                placeholder
                    .filter(|&r| inside(r, p))
                    .map(|r| (p, r))
                    .or_else(|| {
                        layer_order.iter().find_map(|&index| {
                            surfaces[index]
                                .bounds
                                .filter(|&r| inside(r, p))
                                .map(|r| (p, r))
                        })
                    })
            });
            let pointer = hovered.map(|(p, _)| p);
            let now = Instant::now();
            let dt = now.duration_since(last_camera_tick).as_secs_f64();
            last_camera_tick = now;
            // The high bit covers a held drag; the low bit can catch a tap between frames.
            let button_action = [0x01, 0x02, 0x04, 0x05, 0x06].into_iter()
                .fold(false, |pressed, key| (unsafe { GetAsyncKeyState(key) } != 0) || pressed);
            camera.update_with_buttons(bounds, pointer, hovered.map(|(_, r)| r), dt, button_action);
            let fill = if preview_only {
                &cached_background
            } else {
                &background_pixels
            };
            composite.copy_from_slice(fill);
            for &layer in layer_order.iter().rev() {
                let surface = &surfaces[layer];
                if let Some((bounds, size)) = surface.bounds.zip(surface.size) {
                    if let Some(blit) = camera.blit(bounds, size) {
                        if panel == Some(surface.hwnd) {
                            composite_surface(
                                blit,
                                size,
                                &surface.pixels,
                                render_size,
                                &mut composite,
                                true,
                            );
                        } else {
                            composite_frame(
                                blit,
                                size,
                                &surface.pixels,
                                render_size,
                                &mut composite,
                            );
                        }
                    }
                }
            }
            if let Some(blit) = placeholder.and_then(|r| camera.blit(r, render_size)) {
                composite_frame(blit, render_size, fill, render_size, &mut composite);
            }
            if let Some(position) = pointer {
                if cursor_trail {
                    let now = Instant::now();
                    let lifetime = Duration::from_millis(450);
                    while cursor_history
                        .front()
                        .is_some_and(|(time, _)| now.duration_since(*time) >= lifetime)
                    {
                        cursor_history.pop_front();
                    }
                    let mut previous = None;
                    for (time, point) in cursor_history
                        .iter()
                        .chain(std::iter::once(&(now, position)))
                    {
                        let projected = camera.project(*point);
                        if let (Some(from), Some(to)) = (previous, projected) {
                            let age = now.duration_since(*time).as_secs_f64();
                            let opacity = ((1.0 - age / lifetime.as_secs_f64()) * 220.0) as u8;
                            draw_trail_segment(
                                from,
                                to,
                                render_size,
                                &mut composite,
                                opacity,
                                cursor_color,
                            );
                        }
                        previous = projected;
                    }
                    if cursor_history.back().is_none_or(|(_, previous)| {
                        (previous.x - position.x).hypot(previous.y - position.y) >= 4.0
                    }) {
                        cursor_history.push_back((now, position));
                    }
                }
                if let Some(projected) = camera.project(position) {
                    draw_cursor(projected, render_size, &mut composite, 6, 255, cursor_color);
                    let highlight = cursor_color.map(|channel| channel + (255 - channel) / 2);
                    draw_cursor(projected, render_size, &mut composite, 3, 255, highlight);
                }
            } else {
                cursor_history.clear();
            }
            let f8 = key_down(VK_F8.0);
            if let Some(view) = view.as_mut() {
                if !preview_only && f8 && !f8_down {
                    view.toggle();
                }
                view.pump();
                if preview_only || index % 2 == 0 {
                    let minimum = hovered.and_then(|(p, r)| camera.minimum_guide(p, r));
                    view.draw(&composite, render_size, minimum);
                }
            }
            f8_down = f8;
            if let Some(input) = input.as_mut() {
                input.write_all(&composite)?;
            }
            count += 1;
        }
        Ok(count)
    })();
    let media_elapsed = media_started.elapsed().as_secs_f64();
    let save_started = Instant::now();
    if !preview_only {
        save_progress(4);
    }
    drop(input);
    let audio_result = tracks
        .iter_mut()
        .map(|track| track.finish())
        .collect::<Vec<_>>();
    let audio_stop_ms = save_started.elapsed().as_millis();
    let Some(mut child) = child else {
        return result.map(|_| ());
    };
    save_progress(14);
    if result.is_err() {
        let _ = child.kill();
    }
    let budget = Duration::from_secs_f64((20.0 + media_elapsed / 4.0).min(180.0));
    let status = wait_stage(&mut child, "视频收尾", budget, &temp)?;
    let video_finish_ms = save_started.elapsed().as_millis() - audio_stop_ms;
    let duration = progress
        .and_then(|task| task.join().ok().flatten())
        .filter(|&us| us > 0)
        .map_or(media_elapsed, |us| us as f64 / 1_000_000.0);
    save_progress(26);
    let count = result?;
    for result in audio_result {
        result?;
    }
    if !status.success() || count == 0 || std::fs::metadata(&temp)?.len() == 0 {
        return Err(format!("MP4 was not saved. FFmpeg exited with {status}").into());
    }
    let usable_tracks = tracks
        .iter()
        .filter(|track| std::fs::metadata(&track.path).is_ok_and(|entry| entry.len() > 4))
        .collect::<Vec<_>>();
    let mut audio_encode_ms = 0;
    let mut mux_ms = 0;
    if usable_tracks.is_empty() {
        save_progress(94);
        std::fs::rename(&temp, path)?;
    } else {
        let encoded_audio = path.with_extension("audio.partial.m4a");
        let muxed = path.with_extension("mux.partial.mp4");
        let seconds = format!("{:.6}", duration.max(1.0 / f64::from(FPS)));
        let padding = format!("apad=whole_dur={seconds}");
        let mut encode = save_command(&args[1]);
        for track in &usable_tracks {
            encode
                .args(["-f", "s16le", "-ar", "48000", "-ac", "2", "-i"])
                .arg(&track.path);
        }
        if usable_tracks.len() == 1 {
            encode.args(["-map", "0:a:0", "-af", &padding]);
        } else {
            encode.args([
                "-filter_complex",
                &format!("[0:a][1:a]amix=inputs=2:duration=longest,{padding}[a]"),
                "-map",
                "[a]",
            ]);
        }
        let started = Instant::now();
        let mut audio_child = encode
            .args(["-t", &seconds, "-c:a", "aac"])
            .arg(&encoded_audio)
            .spawn()?;
        let audio_progress = follow_save_progress(&mut audio_child, duration, 26, 76);
        require_output(&mut audio_child, "音频编码", budget, &encoded_audio, &temp)?;
        if let Some(task) = audio_progress {
            let _ = task.join();
        }
        save_progress(76);
        audio_encode_ms = started.elapsed().as_millis();
        let started = Instant::now();
        let mut mux = save_command(&args[1]);
        let mut mux_child = mux
            .arg("-i")
            .arg(&temp)
            .arg("-i")
            .arg(&encoded_audio)
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy"])
            .arg(&muxed)
            .spawn()?;
        let mux_progress = follow_save_progress(&mut mux_child, duration, 76, 98);
        require_output(&mut mux_child, "MP4 封装", budget, &muxed, &temp)?;
        if let Some(task) = mux_progress {
            let _ = task.join();
        }
        mux_ms = started.elapsed().as_millis();
        std::fs::rename(&muxed, path)?;
        let _ = std::fs::remove_file(&encoded_audio);
        let _ = std::fs::remove_file(&temp);
    }
    for track in &tracks {
        let _ = std::fs::remove_file(&track.path);
    }
    let _ = std::fs::write(
        path.with_extension("save.log"),
        format!(
            "audio_stop_ms={audio_stop_ms}\nvideo_finish_ms={video_finish_ms}\naudio_encode_ms={audio_encode_ms}\nmux_ms={mux_ms}\ntotal_ms={}\n",
            save_started.elapsed().as_millis()
        ),
    );
    save_progress(100);
    println!("Saved {count} frames to {}", path.display());
    Ok(())
}

fn save_command(ffmpeg: &str) -> Command {
    let mut command = Command::new(ffmpeg);
    command
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-n",
            "-stats_period",
            "0.15",
            "-progress",
            "pipe:1",
        ]);
    command
}

fn save_progress(percent: u8) {
    let _ = writeln!(std::io::stdout(), "SAVE_PROGRESS {percent}");
}

fn follow_save_progress(
    child: &mut Child,
    duration: f64,
    from: u8,
    to: u8,
) -> Option<std::thread::JoinHandle<()>> {
    let stdout = child.stdout.take()?;
    Some(std::thread::spawn(move || {
        let mut last = from;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|amount| amount.parse::<u64>().ok())
            else {
                continue;
            };
            let portion = (us as f64 / 1_000_000.0 / duration.max(0.001)).clamp(0., 1.);
            let progress = from + (portion * f64::from(to - from)).floor() as u8;
            if progress > last {
                save_progress(progress);
                last = progress;
            }
        }
    }))
}

fn wait_stage(
    child: &mut Child,
    label: &str,
    budget: Duration,
    recovery: &Path,
) -> Result<std::process::ExitStatus, AnyError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= budget {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{label}超时，已保留中间文件：{}", recovery.display()).into());
        }
        sleep(Duration::from_millis(25));
    }
}

fn require_output(
    child: &mut Child,
    label: &str,
    budget: Duration,
    path: &Path,
    recovery: &Path,
) -> Result<(), AnyError> {
    let status = wait_stage(child, label, budget, recovery)?;
    if !status.success() || !std::fs::metadata(path).is_ok_and(|entry| entry.len() > 0) {
        return Err(format!("{label}失败，已保留中间文件：{}", recovery.display()).into());
    }
    Ok(())
}

fn timestamp_input(ffmpeg: &mut Command, offset: &str) {
    ffmpeg.args(["-use_wallclock_as_timestamps", "1", "-itsoffset", offset]);
}

fn key_down(key: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

struct Surface {
    hwnd: HWND,
    monitor: Option<HMONITOR>,
    region: Option<Rect>,
    capture: WindowCapture,
    readback: Readback,
    pixels: Vec<u8>,
    uncropped: Vec<u8>,
    size: Option<Size>,
    bounds: Option<Rect>,
    frame_bounds: Option<Rect>,
}

impl Surface {
    fn start(
        hwnd: HWND,
        monitor: Option<HMONITOR>,
        region: Option<RECT>,
    ) -> windows::core::Result<Self> {
        let monitor = if hwnd.is_invalid() {
            region
                .as_ref()
                .map(|r| unsafe { MonitorFromRect(r, MONITOR_DEFAULTTONEAREST) })
                .or(monitor)
        } else {
            None
        };
        let region = region.map(|r| Rect {
            x: f64::from(r.left),
            y: f64::from(r.top),
            width: f64::from(r.right - r.left),
            height: f64::from(r.bottom - r.top),
        });
        let capture = match monitor {
            Some(monitor) => WindowCapture::start_display(monitor)?,
            None => WindowCapture::start(hwnd)?,
        };
        Ok(Self {
            hwnd,
            monitor,
            region,
            capture,
            readback: Readback::default(),
            pixels: Vec::new(),
            uncropped: Vec::new(),
            size: None,
            bounds: None,
            frame_bounds: None,
        })
    }

    fn poll(&mut self) -> windows::core::Result<()> {
        let bounds = surface_bounds(self.hwnd, self.monitor);
        let mut available = bounds;
        if let Some(region) = self.region {
            available = bounds.and_then(|bounds| intersection(region, bounds));
            if bounds != self.frame_bounds {
                self.size = None;
            }
        }
        match self.capture.poll()? {
            Poll::Frame(frame) if available.is_some() => {
                let size = frame.geometry(None).size;
                self.frame_bounds = bounds;
                if self.region.is_some() {
                    frame.with_texture(|texture| {
                        self.readback
                            .copy(texture, size.width, size.height, &mut self.uncropped)
                    })??;
                    self.size = crop_region(
                        bounds.unwrap(),
                        available.unwrap(),
                        size,
                        &self.uncropped,
                        &mut self.pixels,
                    );
                } else {
                    frame.with_texture(|texture| {
                        self.readback
                            .copy(texture, size.width, size.height, &mut self.pixels)
                    })??;
                    self.size = Some(size);
                }
            }
            Poll::Pending => {}
            _ => self.size = None,
        }
        if available.is_none() {
            self.size = None;
        }
        self.bounds = available.filter(|_| self.size.is_some());
        Ok(())
    }
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let w = (a.x + a.width).min(b.x + b.width) - x;
    let h = (a.y + a.height).min(b.y + b.height) - y;
    (w > 0. && h > 0.).then_some(Rect {
        x,
        y,
        width: w,
        height: h,
    })
}

fn crop_region(
    monitor: Rect,
    region: Rect,
    size: Size,
    full: &[u8],
    output: &mut Vec<u8>,
) -> Option<Size> {
    let scale_x = f64::from(size.width) / monitor.width;
    let scale_y = f64::from(size.height) / monitor.height;
    let left = ((region.x - monitor.x) * scale_x).ceil() as usize;
    let top = ((region.y - monitor.y) * scale_y).ceil() as usize;
    let right = ((region.x + region.width - monitor.x) * scale_x)
        .floor()
        .min(f64::from(size.width)) as usize;
    let bottom = ((region.y + region.height - monitor.y) * scale_y)
        .floor()
        .min(f64::from(size.height)) as usize;
    if left >= right || top >= bottom {
        return None;
    }
    let width = right - left;
    let pitch = size.width as usize * 4;
    output.clear();
    output.reserve(width * (bottom - top) * 4);
    for y in top..bottom {
        output.extend_from_slice(&full[y * pitch + left * 4..y * pitch + right * 4]);
    }
    Some(Size {
        width: width as u32,
        height: (bottom - top) as u32,
    })
}

fn desktop_pointer() -> Option<Point> {
    let mut position = POINT::default();
    unsafe {
        GetCursorPos(&mut position).ok()?;
    }
    Some(Point {
        x: f64::from(position.x),
        y: f64::from(position.y),
    })
}

fn surface_bounds(hwnd: HWND, monitor: Option<HMONITOR>) -> Option<Rect> {
    let mut bounds = RECT::default();
    unsafe {
        if let Some(monitor) = monitor {
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            bounds = info.rcMonitor;
        } else {
            if !IsWindow(Some(hwnd)).as_bool()
                || !IsWindowVisible(hwnd).as_bool()
                || IsIconic(hwnd).as_bool()
            {
                return None;
            }
            GetWindowRect(hwnd, &mut bounds).ok()?;
        }
    }
    let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(Rect {
        x: f64::from(bounds.left),
        y: f64::from(bounds.top),
        width: f64::from(width),
        height: f64::from(height),
    })
}

fn inside(rect: Rect, p: Point) -> bool {
    p.x >= rect.x && p.x < rect.x + rect.width && p.y >= rect.y && p.y < rect.y + rect.height
}

fn refresh_layer_order(surfaces: &[Surface], order: &mut Vec<usize>) {
    order.clear();
    if surfaces.len() == 1 {
        order.push(0);
        return;
    }
    let mut window = unsafe { GetTopWindow(None) }.ok();
    // Win32 returns top-level windows front to back.
    for _ in 0..4096 {
        let Some(hwnd) = window else {
            break;
        };
        if let Some(index) = surfaces.iter().position(|surface| surface.hwnd == hwnd) {
            if !order.contains(&index) {
                order.push(index);
            }
            if order.len() == surfaces.len() {
                return;
            }
        }
        window = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
    }
    for index in 0..surfaces.len() {
        if !order.contains(&index) {
            order.push(index);
        }
    }
}

fn join_rects(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

fn composite_frame(blit: Blit, size: Size, source: &[u8], extent: Size, output: &mut [u8]) {
    composite_surface(blit, size, source, extent, output, false);
}

fn composite_surface(
    blit: Blit,
    size: Size,
    source: &[u8],
    extent: Size,
    output: &mut [u8],
    panel: bool,
) {
    let width = extent.width as usize;
    let (src, dst) = (blit.source, blit.destination);
    let (x0, y0) = (dst.x.ceil() as usize, dst.y.ceil() as usize);
    let x1 = ((dst.x + dst.width).ceil() as usize).min(width);
    let y1 = ((dst.y + dst.height).ceil() as usize).min(extent.height as usize);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let scale_x = src.width / dst.width;
    let scale_y = src.height / dst.height;
    let source_width = size.width as usize;
    let source_height = size.height as usize;
    if !panel && (scale_x - 1.0).abs() < 1e-6 && (scale_y - 1.0).abs() < 1e-6 {
        let left = src.x + x0 as f64 - dst.x;
        let top = src.y + y0 as f64 - dst.y;
        let sx = left.round() as usize;
        let sy = top.round() as usize;
        if (left - left.round()).abs() < 1e-6
            && (top - top.round()).abs() < 1e-6
            && sx + x1 - x0 <= source_width
            && sy + y1 - y0 <= source_height
        {
            let bytes = (x1 - x0) * 4;
            for y in y0..y1 {
                let from = ((sy + y - y0) * source_width + sx) * 4;
                let to = (y * width + x0) * 4;
                output[to..to + bytes].copy_from_slice(&source[from..from + bytes]);
            }
            return;
        }
    }
    let taps = (x0..x1)
        .map(|x| {
            let (left, right, weight) = sample_axis(
                src.x + (x as f64 + 0.5 - dst.x) * scale_x - 0.5,
                source_width,
            );
            (left * 4, right * 4, weight)
        })
        .collect::<Vec<_>>();
    for y in y0..y1 {
        let (top, bottom, wy) = sample_axis(
            src.y + (y as f64 + 0.5 - dst.y) * scale_y - 0.5,
            source_height,
        );
        let pitch = source_width * 4;
        let top = &source[top * pitch..][..pitch];
        let bottom = &source[bottom * pitch..][..pitch];
        let start = (y * width + x0) * 4;
        let end = (y * width + x1) * 4;
        for (pixel, &(left, right, wx)) in output[start..end].chunks_exact_mut(4).zip(&taps) {
            if panel {
                let weights = [
                    (256 - wx) * (256 - wy),
                    wx * (256 - wy),
                    (256 - wx) * wy,
                    wx * wy,
                ];
                let samples = [
                    &top[left..][..4],
                    &top[right..][..4],
                    &bottom[left..][..4],
                    &bottom[right..][..4],
                ];
                let alpha: u32 = samples
                    .iter()
                    .zip(weights)
                    .map(|(sample, weight)| {
                        // Some capture drivers report transparent WebView pixels as opaque black.
                        if sample[..3] == [0, 0, 0] {
                            0
                        } else {
                            u32::from(sample[3]) * weight
                        }
                    })
                    .sum();
                for c in 0..3 {
                    let color: u32 = samples
                        .iter()
                        .zip(weights)
                        .map(|(sample, weight)| u32::from(sample[c].min(sample[3])) * weight)
                        .sum();
                    pixel[c] = ((color + u32::from(pixel[c]) * (255 * 65536 - alpha) / 255 + 32768)
                        >> 16)
                        .min(255) as u8;
                }
                pixel[3] = 255;
                continue;
            }
            for c in 0..3 {
                let lo = u32::from(top[left + c]) * (256 - wx) + u32::from(top[right + c]) * wx;
                let hi =
                    u32::from(bottom[left + c]) * (256 - wx) + u32::from(bottom[right + c]) * wx;
                pixel[c] = ((lo * (256 - wy) + hi * wy + 32768) >> 16) as u8;
            }
            pixel[3] = 255;
        }
    }
}

fn sample_axis(position: f64, extent: usize) -> (usize, usize, u32) {
    let position = position.clamp(0., (extent - 1) as f64);
    let low = position.floor() as usize;
    (
        low,
        (low + 1).min(extent - 1),
        ((position - low as f64) * 256.).round() as u32,
    )
}

fn draw_trail_segment(
    from: Point,
    to: Point,
    extent: Size,
    output: &mut [u8],
    opacity: u8,
    color: [u8; 3],
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let distance = dx.hypot(dy);
    if !(2.0..=f64::from(extent.width.max(extent.height)) / 3.0).contains(&distance) {
        return;
    }
    let steps = (distance / 5.0).ceil() as u32;
    for step in 0..steps {
        let t = f64::from(step) / f64::from(steps);
        draw_cursor(
            Point {
                x: from.x + dx * t,
                y: from.y + dy * t,
            },
            extent,
            output,
            4,
            opacity,
            color,
        );
    }
}

fn draw_cursor(
    p: Point,
    extent: Size,
    output: &mut [u8],
    radius: i32,
    opacity: u8,
    color: [u8; 3],
) {
    let cx = p.x as i32;
    let cy = p.y as i32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let (x, y) = (cx + dx, cy + dy);
            if dx * dx + dy * dy > radius * radius
                || x < 0
                || x >= extent.width as i32
                || y < 0
                || y >= extent.height as i32
            {
                continue;
            }
            let to = (y as usize * extent.width as usize + x as usize) * 4;
            let alpha = u16::from(opacity);
            for (pixel, color) in output[to..to + 3].iter_mut().zip(color) {
                *pixel =
                    ((u16::from(*pixel) * (255 - alpha) + u16::from(color) * alpha) / 255) as u8;
            }
            output[to + 3] = 255;
        }
    }
}

fn preview_render_size(base: Size, client_long_edge: u32) -> Size {
    let mut a = base.width;
    let mut b = base.height;
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let (w, h) = (base.width / a, base.height / a);
    let long_edge = w.max(h);
    let requested = client_long_edge.min(1920).div_ceil(128) * 128;
    let maximum = (1920 / long_edge).max(a);
    let factor = requested.div_ceil(long_edge).clamp(a, maximum);
    Size {
        width: w * factor,
        height: h * factor,
    }
}

fn resize_background(pixels: &[u8], from: Size, to: Size) -> Vec<u8> {
    let extent = |s: Size| Rect {
        x: 0.0,
        y: 0.0,
        width: f64::from(s.width),
        height: f64::from(s.height),
    };
    let mut data = vec![0; to.width as usize * to.height as usize * 4];
    composite_frame(
        Blit {
            source: extent(from),
            destination: extent(to),
        },
        from,
        pixels,
        to,
        &mut data,
    );
    data
}

struct Preview {
    hwnd: HWND,
    visible: bool,
    output: Size,
    guide_pixels: Vec<u8>,
    size_file: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct PreviewDrag {
    start: POINT,
    bounds: RECT,
    edge: (i32, i32),
}

fn preview_edge(cursor: POINT, bounds: RECT) -> (i32, i32) {
    const GRIP: i32 = 12;
    let side = |p, low, high| {
        if p - low < GRIP {
            -1
        } else if high - p <= GRIP {
            1
        } else {
            0
        }
    };
    (
        side(cursor.x, bounds.left, bounds.right),
        side(cursor.y, bounds.top, bounds.bottom),
    )
}

impl PreviewDrag {
    fn placement(self, cursor: POINT, output: Size) -> (i32, i32, i32, i32) {
        let (dx, dy) = (cursor.x - self.start.x, cursor.y - self.start.y);
        let (horizontal, vertical) = self.edge;
        let w = self.bounds.right - self.bounds.left;
        let h = self.bounds.bottom - self.bounds.top;
        if self.edge == (0, 0) {
            return (self.bounds.left + dx, self.bounds.top + dy, w, h);
        }
        let ratio = f64::from(output.width) / f64::from(output.height);
        let delta_x = f64::from(horizontal * dx) / ratio;
        let delta_y = f64::from(vertical * dy);
        let delta = if vertical == 0 || (horizontal != 0 && delta_x.abs() >= delta_y.abs()) {
            delta_x
        } else {
            delta_y
        };
        let max_height = 4096.0 / ratio.max(1.0);
        let min_height = (96.0 * (1.0 / ratio).max(1.0)).min(max_height);
        let height = (f64::from(h) + delta).clamp(min_height, max_height);
        let nw = (height * ratio).round() as i32;
        let nh = height.round() as i32;
        let anchored = |start, old, new, side| match side {
            -1 => start + old - new,
            1 => start,
            _ => start + (old - new) / 2,
        };
        (
            anchored(self.bounds.left, w, nw, horizontal),
            anchored(self.bounds.top, h, nh, vertical),
            nw,
            nh,
        )
    }
}

thread_local! {
    static PREVIEW_DRAG: Cell<Option<PreviewDrag>> = const { Cell::new(None) };
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn transparent_panel_pixels_reveal_the_existing_frame() {
        let size = Size {
            width: 2,
            height: 1,
        };
        let rect = Rect {
            x: 0.,
            y: 0.,
            width: 2.,
            height: 1.,
        };
        let mut frame = [40, 50, 60, 255, 40, 50, 60, 255];
        composite_surface(
            Blit {
                source: rect,
                destination: rect,
            },
            size,
            &[0, 0, 0, 255, 90, 100, 110, 255],
            size,
            &mut frame,
            true,
        );
        assert_eq!(frame, [40, 50, 60, 255, 90, 100, 110, 255]);
    }

    #[test]
    fn trail_segment_marks_pixels_between_cursor_samples() {
        let size = Size {
            width: 50,
            height: 20,
        };
        let mut frame = vec![0; 50 * 20 * 4];
        draw_trail_segment(
            Point { x: 10., y: 10. },
            Point { x: 25., y: 10. },
            size,
            &mut frame,
            180,
            [0, 180, 0],
        );
        let offset = (10 * 50 + 18) * 4;
        assert!(frame[offset + 1] > 0);
        assert_eq!(frame[offset + 2], 0);
    }

    #[test]
    fn scaling_interpolates_instead_of_repeating_pixels() {
        let from = Size {
            width: 2,
            height: 2,
        };
        let to = Size {
            width: 3,
            height: 3,
        };
        let source = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let actual = resize_background(&source, from, to);
        assert_eq!(&actual[16..20], &[128, 128, 128, 255]);
        assert_eq!(resize_background(&source, from, from), source);
    }

    #[test]
    fn preview_starts_beside_panel_and_stays_on_its_monitor() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        let centered = RECT {
            left: -1180,
            top: 250,
            right: -900,
            bottom: 580,
        };
        assert_eq!(
            preview_placement(centered, work, 236, 420),
            (-886, 250, 236, 420)
        );
        let near_edge = RECT {
            left: -310,
            top: 850,
            right: -30,
            bottom: 1180,
        };
        assert_eq!(
            preview_placement(near_edge, work, 236, 420),
            (-560, 620, 236, 420)
        );
    }

    #[test]
    fn enlarged_preview_renders_more_pixels_without_changing_aspect() {
        let base = Size {
            width: 360,
            height: 640,
        };
        assert_eq!(preview_render_size(base, 420), base);
        assert_eq!(
            preview_render_size(base, 900),
            Size {
                width: 576,
                height: 1024
            }
        );
        assert_eq!(
            preview_render_size(base, 2400),
            Size {
                width: 1080,
                height: 1920
            }
        );
    }

    #[test]
    fn resizing_keeps_ratio_and_opposite_edge() {
        let output = Size {
            width: 1080,
            height: 1920,
        };
        let bounds = RECT {
            left: 40,
            top: 70,
            right: 220,
            bottom: 390,
        };
        let start = POINT { x: 41, y: 71 };
        let drag = PreviewDrag {
            start,
            bounds,
            edge: preview_edge(start, bounds),
        };
        assert_eq!(
            drag.placement(POINT { x: -49, y: -89 }, output),
            (-50, -90, 270, 480)
        );
        let start = POINT { x: 219, y: 200 };
        let drag = PreviewDrag {
            start,
            bounds,
            edge: preview_edge(start, bounds),
        };
        assert_eq!(
            drag.placement(POINT { x: 309, y: 200 }, output),
            (40, -10, 270, 480)
        );
    }
}

unsafe extern "system" fn preview_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_LBUTTONDOWN || message == WM_SETCURSOR {
        let (mut cursor, mut bounds) = (POINT::default(), RECT::default());
        if unsafe { GetCursorPos(&mut cursor).is_ok() && GetWindowRect(hwnd, &mut bounds).is_ok() }
        {
            let edge = preview_edge(cursor, bounds);
            if message == WM_LBUTTONDOWN {
                PREVIEW_DRAG.set(Some(PreviewDrag {
                    start: cursor,
                    bounds,
                    edge,
                }));
                return LRESULT(0);
            }
            let edge = PREVIEW_DRAG.get().map_or(edge, |drag| drag.edge);
            let icon = match edge {
                (0, 0) => None,
                (0, _) => Some(IDC_SIZENS),
                (_, 0) => Some(IDC_SIZEWE),
                (-1, -1) | (1, 1) => Some(IDC_SIZENWSE),
                _ => Some(IDC_SIZENESW),
            };
            if let Some(icon) = icon {
                if let Ok(cursor) = unsafe { LoadCursorW(None, icon) } {
                    unsafe {
                        SetCursor(Some(cursor));
                    }
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn preview_placement(panel: RECT, work: RECT, width: i32, height: i32) -> (i32, i32, i32, i32) {
    const GAP: i32 = 14;
    let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
    let scale = (f64::from(work_w) / f64::from(width))
        .min(f64::from(work_h) / f64::from(height))
        .min(1.);
    let w = (f64::from(width) * scale).floor().max(1.) as i32;
    let h = (f64::from(height) * scale).floor().max(1.) as i32;
    let (right, left) = (panel.right + GAP, panel.left - GAP - w);
    let (below, above) = (panel.bottom + GAP, panel.top - GAP - h);
    let (x, y) = if right + w <= work.right {
        (right, panel.top)
    } else if left >= work.left {
        (left, panel.top)
    } else if below + h <= work.bottom {
        (panel.left, below)
    } else if above >= work.top {
        (panel.left, above)
    } else if work.right - panel.right >= panel.left - work.left {
        (right, panel.top)
    } else {
        (left, panel.top)
    };
    (
        x.clamp(work.left, (work.right - w).max(work.left)),
        y.clamp(work.top, (work.bottom - h).max(work.top)),
        w,
        h,
    )
}

fn place_near_panel(panel: HWND, width: i32, height: i32) -> Option<(i32, i32, i32, i32)> {
    let mut bounds = RECT::default();
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetWindowRect(panel, &mut bounds).ok()?;
        let monitor = MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
    }
    Some(preview_placement(bounds, info.rcWork, width, height))
}

impl Preview {
    fn new(
        output: Size,
        visible: bool,
        capture_display: bool,
        near: Option<HWND>,
        size_file: Option<PathBuf>,
    ) -> windows::core::Result<Self> {
        let scale = size_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|edge| (96..=4096).contains(edge))
            .map(|edge| f64::from(edge) / f64::from(output.width.max(output.height)))
            .unwrap_or_else(|| {
                (420.0 / f64::from(output.height)).min(480.0 / f64::from(output.width))
            });
        let width = (f64::from(output.width) * scale).round() as i32;
        let height = (f64::from(output.height) * scale).round() as i32;
        let (x, y, width, height) = near
            .and_then(|panel| place_near_panel(panel, width, height))
            .unwrap_or((40, 40, width, height));
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                x,
                y,
                width,
                height,
                None,
                None,
                None,
                None,
            )?
        };
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                preview_window_proc as *const () as isize,
            );
        }
        if capture_display {
            if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(error);
            }
        }
        if visible {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNA);
            }
        }
        Ok(Self {
            hwnd,
            visible,
            output,
            guide_pixels: Vec::new(),
            size_file,
        })
    }

    fn toggle(&mut self) {
        PREVIEW_DRAG.set(None);
        self.visible = !self.visible;
        unsafe {
            let _ = ShowWindow(self.hwnd, if self.visible { SW_SHOWNA } else { SW_HIDE });
        }
    }

    fn pump(&self) {
        let mut message = MSG::default();
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            // Move between frames without entering Windows' modal move loop.
            if !self.visible || !key_down(VK_LBUTTON.0) {
                if PREVIEW_DRAG.take().is_some_and(|drag| drag.edge != (0, 0)) {
                    self.save_size();
                }
            } else if let Some(drag) = PREVIEW_DRAG.get() {
                let mut cursor = POINT::default();
                if GetCursorPos(&mut cursor).is_ok() {
                    let (x, y, width, height) = drag.placement(cursor, self.output);
                    let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
                    if drag.edge == (0, 0) {
                        flags |= SWP_NOSIZE;
                    }
                    let _ = SetWindowPos(self.hwnd, None, x, y, width, height, flags);
                }
            }
        }
    }

    fn save_size(&self) {
        let Some((path, edge)) = self.size_file.as_ref().zip(self.client_long_edge()) else {
            return;
        };
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let result = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
            .and_then(|()| std::fs::write(&temporary, edge.to_string()))
            .and_then(|()| std::fs::rename(&temporary, path));
        if let Err(error) = result {
            let _ = std::fs::remove_file(temporary);
            eprintln!("Could not save the preview size: {error}");
        }
    }

    fn client_long_edge(&self) -> Option<u32> {
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(self.hwnd, &mut rect).ok()?;
        }
        u32::try_from(rect.right.max(rect.bottom))
            .ok()
            .filter(|&edge| edge > 0)
    }

    fn draw(&mut self, data: &[u8], render_size: Size, minimum: Option<Rect>) {
        if !self.visible {
            return;
        }
        let data = if let Some(rect) = minimum {
            self.guide_pixels.clear();
            self.guide_pixels.extend_from_slice(data);
            draw_minimum(rect, render_size, &mut self.guide_pixels);
            &self.guide_pixels
        } else {
            data
        };
        let mut rect = RECT::default();
        unsafe {
            if GetClientRect(self.hwnd, &mut rect).is_err() {
                return;
            }
            let dc = GetDC(Some(self.hwnd));
            if dc.is_invalid() {
                return;
            }
            SetStretchBltMode(dc, HALFTONE);
            let _ = SetBrushOrgEx(dc, 0, 0, None);
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: render_size.width as i32,
                biHeight: -(render_size.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            StretchDIBits(
                dc,
                0,
                0,
                rect.right,
                rect.bottom,
                0,
                0,
                render_size.width as i32,
                render_size.height as i32,
                Some(data.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            ReleaseDC(Some(self.hwnd), dc);
        }
    }
}

fn draw_minimum(rect: Rect, size: Size, pixels: &mut [u8]) {
    let (x0, x1) = (rect.x.round() as i32, (rect.x + rect.width).round() as i32);
    let (y0, y1) = (rect.y.round() as i32, (rect.y + rect.height).round() as i32);
    let (w, h) = (size.width as i32, size.height as i32);
    let unit = (w.min(h) / 90).max(3);
    let thickness = (unit / 3).max(1);
    let mut paint = |x: i32, y: i32| {
        if x >= 0 && x < w && y >= 0 && y < h {
            let at = (y as usize * w as usize + x as usize) * 4;
            pixels[at..at + 4].copy_from_slice(&[68, 202, 255, 255]);
        }
    };
    for x in x0.max(0)..=x1.min(w - 1) {
        if (x - x0) % (2 * unit) < unit {
            for offset in 0..thickness {
                paint(x, y0 + offset);
                paint(x, y1 - offset);
            }
        }
    }
    for y in y0.max(0)..=y1.min(h - 1) {
        if (y - y0) % (2 * unit) < unit {
            for offset in 0..thickness {
                paint(x0 + offset, y);
                paint(x1 - offset, y);
            }
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
