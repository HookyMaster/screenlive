use std::cell::Cell;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BLACK_BRUSH, BeginPaint, CreatePen, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
            GetStockObject, HBRUSH, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
            MonitorFromWindow, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, Rectangle, SelectObject,
        },
        UI::{
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_WNDPROC,
                GetCursorPos, GetMessageW, IDC_CROSS, LWA_ALPHA, LoadCursorW, MSG, PostQuitMessage,
                SW_SHOW, SetCursor, SetForegroundWindow, SetLayeredWindowAttributes,
                SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_DESTROY, WM_KEYDOWN,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR,
                WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::w,
};

thread_local! {
    static START: Cell<Option<POINT>> = const { Cell::new(None) };
    static END: Cell<Option<POINT>> = const { Cell::new(None) };
    static RESULT: Cell<Option<RECT>> = const { Cell::new(None) };
    static MONITOR: Cell<RECT> = const { Cell::new(RECT { left: 0, top: 0, right: 0, bottom: 0 }) };
}

fn current() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe {
        GetCursorPos(&mut p).ok()?;
    }
    let bounds = MONITOR.get();
    Some(POINT {
        x: p.x.clamp(bounds.left, bounds.right),
        y: p.y.clamp(bounds.top, bounds.bottom),
    })
}

fn selected() -> Option<RECT> {
    let (a, b) = (START.get()?, END.get()?);
    Some(RECT {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x),
        bottom: a.y.max(b.y),
    })
}

unsafe extern "system" fn picker(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_LBUTTONDOWN => {
            let p = current();
            START.set(p);
            END.set(p);
            unsafe {
                SetCapture(hwnd);
            }
        }
        WM_MOUSEMOVE if START.get().is_some() => {
            END.set(current());
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_LBUTTONUP if START.get().is_some() => {
            END.set(current());
            let result = selected().filter(|r| r.right - r.left >= 16 && r.bottom - r.top >= 16);
            unsafe {
                let _ = ReleaseCapture();
            }
            if let Some(rect) = result {
                RESULT.set(Some(rect));
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            } else {
                START.set(None);
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
        }
        WM_RBUTTONDOWN => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        WM_SETCURSOR => {
            if let Ok(cursor) = unsafe { LoadCursorW(None, IDC_CROSS) } {
                unsafe {
                    SetCursor(Some(cursor));
                }
            }
            return LRESULT(1);
        }
        WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_invalid() {
                FillRect(dc, &paint.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                if let Some(r) = selected() {
                    let monitor = MONITOR.get();
                    let pen = CreatePen(
                        PS_SOLID,
                        4,
                        windows::Win32::Foundation::COLORREF(0x00ff_ffff),
                    );
                    let old_pen = SelectObject(dc, pen.into());
                    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                    let _ = Rectangle(
                        dc,
                        r.left - monitor.left,
                        r.top - monitor.top,
                        r.right - monitor.left,
                        r.bottom - monitor.top,
                    );
                    SelectObject(dc, old_brush);
                    SelectObject(dc, old_pen);
                    let _ = DeleteObject(pen.into());
                }
                let _ = EndPaint(hwnd, &paint);
            }
        },
        WM_DESTROY => unsafe {
            PostQuitMessage(0);
        },
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

pub fn select(owner: HWND) -> windows::core::Result<Option<RECT>> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let display = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(display, &mut info).ok()?;
        let r = info.rcMonitor;
        MONITOR.set(r);
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("STATIC"),
            w!("ScreenLive 框选范围"),
            WS_POPUP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            None,
            None,
        )?;
        SetWindowLongPtrW(window, GWLP_WNDPROC, picker as *const () as isize);
        if let Err(error) = SetLayeredWindowAttributes(
            window,
            windows::Win32::Foundation::COLORREF(0),
            130,
            LWA_ALPHA,
        ) {
            let _ = DestroyWindow(window);
            return Err(error);
        }
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(RESULT.get())
}
