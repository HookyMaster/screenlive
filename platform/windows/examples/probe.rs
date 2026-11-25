use std::{
    thread,
    time::{Duration, Instant},
};

use screenlive_core::{FramingSession, Mode, Size};
use screenlive_windows::{Poll, WindowCapture};
use windows::Win32::Foundation::HWND;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = std::env::args()
        .nth(1)
        .ok_or("Usage: probe 0xWINDOW_HANDLE")?;
    let address = usize::from_str_radix(handle.trim_start_matches("0x"), 16)?;
    let mut capture = WindowCapture::start(HWND(address as *mut _))?;
    let output = Size {
        width: 720,
        height: 1280,
    };
    let mut framing = FramingSession::new(capture.source().clone(), output, Mode::Fit)
        .ok_or("Invalid output size")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match capture.poll()? {
            Poll::Frame(frame) => {
                let geometry = frame.geometry(None);
                let plan = framing.frame(Some(geometry), 1.0 / 30.0);
                frame.with_texture(|_| {
                    println!("GPU frame: {:?}; plan: {:?}", geometry.size, plan)
                })?;
                return Ok(());
            }
            Poll::Lost => return Err("Selected window was closed".into()),
            Poll::Pending | Poll::Resized => thread::sleep(Duration::from_millis(10)),
        }
    }
    Err("No frame received from the selected window within 5 seconds".into())
}
