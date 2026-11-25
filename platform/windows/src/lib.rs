//! Windows Graphics Capture backend. Captured surfaces stay on the GPU.
#![cfg(windows)]

pub mod audio;
pub mod background;
mod readback;
pub use readback::Readback;

pub fn parse_region(source: &str) -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Foundation::RECT;
    let coords = source
        .strip_prefix("region:")?
        .split(':')
        .map(str::parse::<i32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    let [left, top, right, bottom]: [i32; 4] = coords.try_into().ok()?;
    let width = right.checked_sub(left)?;
    let height = bottom.checked_sub(top)?;
    (width >= 16 && height >= 16 && width <= 32768 && height <= 32768).then_some(RECT {
        left,
        top,
        right,
        bottom,
    })
}

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use screenlive_core::{CaptureSource, Point, Size, SourceGeometry};
use windows::{
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, HWND, S_OK},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device, ID3D11Texture2D,
            },
            Dxgi::IDXGIDevice,
            Gdi::HMONITOR,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
        },
    },
    core::{Error, Interface, Result, factory},
};

// WinRT initialization and uninitialization must happen on the same thread.
struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);

impl Apartment {
    fn initialize() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(Self(std::marker::PhantomData))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

pub enum Poll<'a> {
    Pending,
    Resized,
    Lost,
    Frame(WindowFrame<'a>),
}

pub struct WindowFrame<'a> {
    source: &'a CaptureSource,
    size: Size,
    frame: Direct3D11CaptureFrame,
}

impl WindowFrame<'_> {
    pub fn geometry(&self, pointer: Option<Point>) -> SourceGeometry<'_> {
        SourceGeometry {
            source: self.source,
            size: self.size,
            pointer,
        }
    }

    /// The callback must finish using the texture before it returns.
    pub fn with_texture<T>(&self, consume: impl FnOnce(&ID3D11Texture2D) -> T) -> Result<T> {
        let access: IDirect3DDxgiInterfaceAccess = self.frame.Surface()?.cast()?;
        let texture = unsafe { access.GetInterface::<ID3D11Texture2D>()? };
        Ok(consume(&texture))
    }
}

pub struct WindowCapture {
    source: CaptureSource,
    item: GraphicsCaptureItem,
    close_token: i64,
    lost: Arc<AtomicBool>,
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    device: IDirect3DDevice,
    pool_size: SizeInt32,
    _apartment: Apartment,
}

impl WindowCapture {
    /// Call on a dedicated capture thread. The handle must be the window the
    /// user selected; there is no monitor fallback in this backend.
    pub fn start(window: HWND) -> Result<Self> {
        if window.0.is_null() {
            return Err(Error::new(E_INVALIDARG, "No window was selected"));
        }
        Self::start_item(
            CaptureSource::Window(format!("{:p}", window.0)),
            |interop| unsafe { interop.CreateForWindow(window) },
        )
    }

    pub fn start_display(monitor: HMONITOR) -> Result<Self> {
        if monitor.0.is_null() {
            return Err(Error::new(E_INVALIDARG, "No monitor was selected"));
        }
        Self::start_item(
            CaptureSource::Display(format!("{:p}", monitor.0)),
            |interop| unsafe { interop.CreateForMonitor(monitor) },
        )
    }

    fn start_item(
        source: CaptureSource,
        create: impl FnOnce(&IGraphicsCaptureItemInterop) -> Result<GraphicsCaptureItem>,
    ) -> Result<Self> {
        let apartment = Apartment::initialize()?;
        if !GraphicsCaptureSession::IsSupported()? {
            return Err(Error::new(
                E_NOTIMPL,
                "Windows Graphics Capture is unavailable",
            ));
        }
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item = create(&interop)?;
        let pool_size = item.Size()?;
        let mut d3d = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
        }
        let d3d: ID3D11Device = d3d.ok_or_else(|| Error::new(E_FAIL, "No D3D11 device"))?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        let device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            pool_size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        session.SetIsCursorCaptureEnabled(false)?;
        let lost = Arc::new(AtomicBool::new(false));
        let closed = Arc::clone(&lost);
        let close_token =
            item.Closed(&windows::Foundation::TypedEventHandler::new(move |_, _| {
                closed.store(true, Ordering::Release);
                Ok(())
            }))?;
        let capture = Self {
            source,
            item,
            close_token,
            lost,
            session,
            pool,
            device,
            pool_size,
            _apartment: apartment,
        };
        capture.session.StartCapture()?;
        Ok(capture)
    }

    pub fn source(&self) -> &CaptureSource {
        &self.source
    }

    pub fn poll(&mut self) -> Result<Poll<'_>> {
        if self.lost.load(Ordering::Acquire) {
            return Ok(Poll::Lost);
        }
        let frame = match self.pool.TryGetNextFrame() {
            Ok(frame) => frame,
            // The WinRT method returns null while the queue is empty. The
            // Rust binding converts a null runtime class into an empty error.
            Err(error) if error.code() == S_OK => return Ok(Poll::Pending),
            Err(error) => return Err(error),
        };
        if frame.as_raw().is_null() {
            return Ok(Poll::Pending);
        }
        let size = frame.ContentSize()?;
        if size.Width <= 0 || size.Height <= 0 {
            return Ok(Poll::Pending);
        }
        if size != self.pool_size {
            drop(frame);
            self.pool.Recreate(
                &self.device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            )?;
            self.pool_size = size;
            return Ok(Poll::Resized);
        }
        if self.lost.load(Ordering::Acquire) {
            return Ok(Poll::Lost);
        }
        Ok(Poll::Frame(WindowFrame {
            source: &self.source,
            size: Size {
                width: size.Width as u32,
                height: size.Height as u32,
            },
            frame,
        }))
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let _ = self.item.RemoveClosed(self.close_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_null_window_without_starting_capture() {
        let error = WindowCapture::start(HWND::default()).err().unwrap();
        assert_eq!(error.code(), E_INVALIDARG);
    }
}
