use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{E_ACCESSDENIED, E_FAIL},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
            MMDeviceEnumerator, WAVEFORMATEX, eCapture, eConsole, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ,
                StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
            },
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        },
    },
    core::{Error, HSTRING, Result},
};

const RATE: u64 = 48_000;
const FRAME_BYTES: usize = 4;
type TaskResult<T> = std::result::Result<T, String>;

/// Reads peaks from the same WASAPI format used by the recorder. It never writes audio.
pub struct Meter {
    stop: Arc<AtomicBool>,
    peak: Arc<std::sync::atomic::AtomicU32>,
    error: Arc<std::sync::Mutex<Option<String>>>,
    task: Option<JoinHandle<()>>,
}

impl Meter {
    pub fn start(id: String, loopback: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let error = Arc::new(std::sync::Mutex::new(None));
        let (task_stop, task_peak, task_error) = (stop.clone(), peak.clone(), error.clone());
        let task = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let _apartment = Apartment::new()?;
                let client = open(&id, loopback)?;
                let capture: IAudioCaptureClient = unsafe { client.GetService()? };
                while !task_stop.load(Ordering::Acquire) {
                    while unsafe { capture.GetNextPacketSize()? } != 0 {
                        let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                        unsafe {
                            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                        }
                        if !data.is_null() && flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 {
                            let bytes = unsafe {
                                std::slice::from_raw_parts(data, frames as usize * FRAME_BYTES)
                            };
                            let current = bytes
                                .chunks_exact(2)
                                .map(|b| u32::from(i16::from_le_bytes([b[0], b[1]]).unsigned_abs()))
                                .max()
                                .unwrap_or(0);
                            task_peak.fetch_max(current, Ordering::Relaxed);
                        }
                        unsafe {
                            capture.ReleaseBuffer(frames)?;
                        }
                    }
                    thread::sleep(Duration::from_millis(8));
                }
                unsafe {
                    client.Stop()?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                if let Ok(mut target) = task_error.lock() {
                    *target = Some(if error.code() == E_ACCESSDENIED && !loopback {
                        "麦克风访问被拒绝，请检查 Windows 麦克风权限。".into()
                    } else {
                        format!("电平不可用：{error}")
                    });
                }
            }
        });
        Self {
            stop,
            peak,
            error,
            task: Some(task),
        }
    }

    pub fn read(&self) -> TaskResult<f32> {
        if let Some(error) = self.error.lock().map_err(|e| e.to_string())?.as_ref() {
            return Err(error.clone());
        }
        let amplitude = self.peak.swap(0, Ordering::Relaxed) as f32 / 32768.0;
        Ok(if amplitude > 0.0 {
            (20.0 * amplitude.log10()).max(-60.0)
        } else {
            -60.0
        })
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub fn endpoints(loopback: bool) -> Result<Vec<(String, String)>> {
    let _apartment = Apartment::new()?;
    let flow = if loopback { eRender } else { eCapture };
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let devices = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)? };
    let mut entries = Vec::new();
    if unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }.is_ok() {
        entries.push((
            "default".into(),
            if loopback {
                "系统默认播放设备"
            } else {
                "系统默认麦克风"
            }
            .into(),
        ));
    }
    for index in 0..unsafe { devices.GetCount()? } {
        unsafe {
            let device = devices.Item(index)?;
            let raw_id = device.GetId()?;
            let id = raw_id.to_string();
            CoTaskMemFree(Some(raw_id.0.cast()));
            let id = id?;
            let name = (|| -> Result<String> {
                let store = device.OpenPropertyStore(STGM_READ)?;
                let mut value = store.GetValue(&PKEY_Device_FriendlyName)?;
                let name = PropVariantToStringAlloc(&value);
                let _ = PropVariantClear(&mut value);
                let name = name?;
                let result = name.to_string();
                CoTaskMemFree(Some(name.0.cast()));
                Ok(result?)
            })()
            .unwrap_or_else(|_| format!("音频设备 {}", index + 1));
            entries.push((id, name));
        }
    }
    Ok(entries)
}

pub fn clock() -> Result<u64> {
    let (mut ticks, mut frequency) = (0i64, 0i64);
    unsafe {
        QueryPerformanceCounter(&mut ticks)?;
        QueryPerformanceFrequency(&mut frequency)?;
    }
    if frequency <= 0 || ticks < 0 {
        return Err(Error::new(E_FAIL, "Invalid performance counter"));
    }
    Ok((ticks as u128 * 10_000_000 / frequency as u128) as u64)
}

pub struct Track {
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<TaskResult<()>>>,
    pub path: PathBuf,
}

impl Track {
    pub fn start(id: String, loopback: bool, path: PathBuf, epoch: u64) -> TaskResult<Self> {
        let (ready, started) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = Arc::clone(&stop);
        let destination = path.clone();
        let task = thread::spawn(move || {
            let _apartment = Apartment::new().map_err(|e| e.to_string())?;
            let client = match open(&id, loopback) {
                Ok(client) => client,
                Err(error) => {
                    let info = if error.code() == E_ACCESSDENIED && !loopback {
                        "麦克风访问被拒绝。请检查 Windows「设置 → 隐私和安全性 → 麦克风」中的桌面应用访问权限。".into()
                    } else {
                        format!(
                            "{}启动失败：{error}",
                            if loopback {
                                "电脑声音"
                            } else {
                                "麦克风"
                            }
                        )
                    };
                    let _ = ready.send(Err(info.clone()));
                    return Err(info);
                }
            };
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|e| e.to_string())?;
            let mut file = BufWriter::new(file);
            let _ = ready.send(Ok(()));
            let result = capture(&client, &mut file, epoch, &task_stop);
            unsafe {
                let _ = client.Stop();
            }
            result
        });
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                task: Some(task),
                path,
            }),
            Ok(Err(error)) => {
                let _ = task.join();
                Err(error)
            }
            Err(_) => task
                .join()
                .map_err(|_| "Audio worker panicked".to_owned())?
                .and(Err("Audio initialization failed".into())),
        }
    }

    pub fn finish(&mut self) -> TaskResult<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.join()
                .map_err(|_| "Audio worker panicked".to_owned())??;
        }
        self.stop.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for Track {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn open(id: &str, loopback: bool) -> Result<IAudioClient> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = if id == "default" {
            enumerator
                .GetDefaultAudioEndpoint(if loopback { eRender } else { eCapture }, eConsole)?
        } else {
            enumerator.GetDevice(&HSTRING::from(id))?
        };
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: RATE as u32,
            nAvgBytesPerSec: RATE as u32 * FRAME_BYTES as u32,
            nBlockAlign: FRAME_BYTES as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
            | if loopback {
                AUDCLNT_STREAMFLAGS_LOOPBACK
            } else {
                0
            };
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None)?;
        client.Start()?;
        Ok(client)
    }
}

fn capture(
    client: &IAudioClient,
    output: &mut impl Write,
    epoch: u64,
    stop: &AtomicBool,
) -> TaskResult<()> {
    const SILENCE: [u8; 4096] = [0; 4096];
    fn pad(output: &mut impl Write, mut bytes: usize) -> std::io::Result<()> {
        while bytes > 0 {
            let n = bytes.min(SILENCE.len());
            output.write_all(&SILENCE[..n])?;
            bytes -= n;
        }
        Ok(())
    }
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|e| e.to_string())?;
    let mut written = 0u64;
    let result = loop {
        let stopping = stop.load(Ordering::Acquire);
        while unsafe { capture.GetNextPacketSize() }.map_err(|e| e.to_string())? != 0 {
            let (mut data, mut frames, mut flags, mut qpc) =
                (std::ptr::null_mut(), 0u32, 0u32, 0u64);
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc)) }
                .map_err(|e| e.to_string())?;
            let result: std::io::Result<()> = (|| {
                let target = if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
                    written
                } else {
                    qpc.saturating_sub(epoch) * RATE / 10_000_000
                };
                if target > written + 96 {
                    pad(output, (target - written) as usize * FRAME_BYTES)?;
                    written = target;
                }
                let skipped = if written > target + 96 {
                    (written - target).min(frames as u64)
                } else {
                    0
                };
                let frames = u64::from(frames) - skipped;
                let bytes = frames as usize * FRAME_BYTES;
                if bytes > 0 {
                    if data.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        pad(output, bytes)?;
                    } else {
                        output.write_all(unsafe {
                            std::slice::from_raw_parts(
                                data.add(skipped as usize * FRAME_BYTES),
                                bytes,
                            )
                        })?;
                    }
                    written += frames;
                }
                Ok(())
            })();
            let release = unsafe { capture.ReleaseBuffer(frames) };
            result.map_err(|e| e.to_string())?;
            release.map_err(|e| e.to_string())?;
        }
        if stopping {
            if written == 0 {
                pad(output, FRAME_BYTES).map_err(|e| e.to_string())?;
            }
            break output.flush().map_err(|e| e.to_string());
        }
        thread::sleep(Duration::from_millis(5));
    };
    result
}
