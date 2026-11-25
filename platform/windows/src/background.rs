use std::os::windows::process::CommandExt;
use std::{path::PathBuf, process::Command};

pub const WALLPAPERS: [(&str, &str, &str); 3] = [
    (
        "windows-classic",
        "Windows 默认壁纸",
        "Web/Wallpaper/Windows/img0.jpg",
    ),
    (
        "windows-dark",
        "Windows 深色壁纸",
        "Web/Wallpaper/Windows/img19.jpg",
    ),
    (
        "windows-lockscreen",
        "Windows 锁屏风景",
        "Web/Screen/img100.jpg",
    ),
];

pub fn wallpaper(id: &str) -> Option<PathBuf> {
    let (_, _, relative) = WALLPAPERS.iter().find(|(key, _, _)| *key == id)?;
    let path = PathBuf::from(std::env::var_os("WINDIR")?).join(relative);
    path.is_file().then_some(path)
}

pub fn render(ffmpeg: &str, id: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let length = width as usize * height as usize * 4;
    if id.starts_with("windows-") {
        let path = wallpaper(id).ok_or("找不到所选 Windows 壁纸")?;
        let filter = format!(
            "scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height}"
        );
        let result = Command::new(ffmpeg)
            .creation_flags(0x08000000)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args([
                "-vf",
                &filter,
                "-frames:v",
                "1",
                "-pix_fmt",
                "bgra",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        if !result.status.success() || result.stdout.len() != length {
            return Err("无法读取所选 Windows 壁纸".into());
        }
        return Ok(result.stdout);
    }
    let (top, bottom): ([u8; 3], [u8; 3]) = match id {
        "gradient" => ([52, 41, 32], [80, 55, 46]),
        "solid" => ([52, 41, 32], [52, 41, 32]),
        "mist" => ([249, 244, 238], [228, 216, 204]),
        "mint" => ([244, 248, 222], [208, 230, 187]),
        "sunset" => ([208, 220, 255], [180, 158, 240]),
        "lavender" => ([253, 237, 248], [237, 199, 207]),
        "apricot" => ([214, 233, 255], [161, 190, 251]),
        "teal" => ([242, 249, 219], [210, 215, 147]),
        "sand" => ([227, 243, 254], [192, 215, 237]),
        _ => return Err("不支持该背景".into()),
    };
    let mut pixels = vec![0; length];
    for (y, row) in pixels.chunks_exact_mut(width as usize * 4).enumerate() {
        let t = y as f32 / (height - 1) as f32;
        let color = std::array::from_fn::<_, 3, _>(|i| {
            (f32::from(top[i]) * (1.0 - t) + f32::from(bottom[i]) * t).round() as u8
        });
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[color[0], color[1], color[2], 255]);
        }
    }
    Ok(pixels)
}
