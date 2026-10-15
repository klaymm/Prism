use slint::winit_030::{WinitWindowAccessor, winit};

pub fn init_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attrs| {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            let icon = winit::window::Icon::from_resource(1, None).ok();
            let centred = centred_position(&attrs);
            let mut attrs = attrs.with_undecorated_shadow(true).with_window_icon(icon.clone()).with_taskbar_icon(icon);
            if let Some(pos) = centred {
                attrs = attrs.with_position(pos);
            }
            attrs
        })
        .select()
}

fn centred_position(attrs: &winit::window::WindowAttributes) -> Option<winit::dpi::PhysicalPosition<i32>> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let size = attrs.inner_size?;
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..std::mem::zeroed() };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x as f64 / 96.0;
        let physical: winit::dpi::PhysicalSize<i32> = size.to_physical(scale);
        let work = info.rcWork;
        let x = work.left + (work.right - work.left - physical.width) / 2;
        let y = work.top + (work.bottom - work.top - physical.height) / 2;
        Some(winit::dpi::PhysicalPosition::new(x, y.max(work.top)))
    }
}

fn hwnd(w: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

fn round_corners(w: &winit::window::Window) {
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
    let Some(hwnd) = hwnd(w) else { return };
    let pref = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(
            hwnd as _,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&pref as *const i32).cast(),
            size_of::<i32>() as u32,
        );
    }
}

pub fn prepare(window: &slint::Window) {
    window.with_winit_window(round_corners);
}

pub fn present(window: &slint::Window) {
    let _ = window.show();
    window.with_winit_window(|w| {
        w.set_minimized(false);
        w.focus_window();
    });
    window.dispatch_event(slint::platform::WindowEvent::PointerExited);
}

pub fn caption_pressed(window: &slint::Window) {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    thread_local!(static LAST_PRESS: Cell<Option<Instant>> = const { Cell::new(None) });
    let limit = Duration::from_millis(unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u64);
    let now = Instant::now();
    if LAST_PRESS.get().is_some_and(|t| now - t <= limit) {
        LAST_PRESS.set(None);
        toggle_maximized(window);
        return;
    }
    LAST_PRESS.set(Some(now));
    window.with_winit_window(|w| {
        let _ = w.drag_window();
    });
}

pub fn drag(window: &slint::Window) {
    window.with_winit_window(|w| {
        let _ = w.drag_window();
    });
}

pub fn animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut enabled: i32 = 1;
    unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut enabled as *mut i32).cast(), 0) };
    enabled != 0
}

pub fn toggle_maximized(window: &slint::Window) {
    window.set_maximized(!window.is_maximized());
}

pub fn minimize(window: &slint::Window) {
    window.dispatch_event(slint::platform::WindowEvent::PointerExited);
    window.set_minimized(true);
}
