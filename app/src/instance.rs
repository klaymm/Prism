use std::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForMultipleObjects,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

const WAIT_OBJECT_0: u32 = 0;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn name(kind: &str) -> Vec<u16> {
    wide(&format!("Local\\{}-{kind}", prism_win::profile().instance_id))
}

struct OpenAccess {
    attrs: SECURITY_ATTRIBUTES,
}

impl OpenAccess {
    fn new() -> Option<Self> {
        let sddl = wide("D:(A;;GA;;;AU)(A;;GA;;;SY)(A;;GA;;;BA)");
        let mut sd: *mut c_void = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut())
        };
        (ok != 0).then(|| Self {
            attrs: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            },
        })
    }

    fn ptr(this: &Option<Self>) -> *const SECURITY_ATTRIBUTES {
        this.as_ref().map_or(std::ptr::null(), |a| &a.attrs)
    }
}

impl Drop for OpenAccess {
    fn drop(&mut self) {
        unsafe { LocalFree(self.attrs.lpSecurityDescriptor) };
    }
}

pub struct Instance {
    _mutex: HANDLE,
    show: HANDLE,
    quit: HANDLE,
}

unsafe impl Send for Instance {}

pub enum Start {
    Primary(Instance),
    Secondary,
}

fn signal(kind: &str) {
    let name = name(kind);
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if !event.is_null() {
            AllowSetForegroundWindow(ASFW_ANY);
            SetEvent(event);
            CloseHandle(event);
        }
    }
}

pub fn acquire(quit: bool) -> Start {
    let access = OpenAccess::new();
    let mutex_name = name("instance");
    let mutex = unsafe { CreateMutexW(OpenAccess::ptr(&access), 0, mutex_name.as_ptr()) };
    let err = unsafe { GetLastError() };
    if mutex.is_null() || err == ERROR_ALREADY_EXISTS || err == ERROR_ACCESS_DENIED {
        if !mutex.is_null() {
            unsafe { CloseHandle(mutex) };
        }
        signal(if quit { "quit" } else { "show" });
        return Start::Secondary;
    }
    let (show_name, quit_name) = (name("show"), name("quit"));
    let show = unsafe { CreateEventW(OpenAccess::ptr(&access), 0, 0, show_name.as_ptr()) };
    let quit_event = unsafe { CreateEventW(OpenAccess::ptr(&access), 0, 0, quit_name.as_ptr()) };
    Start::Primary(Instance { _mutex: mutex, show, quit: quit_event })
}

impl Instance {
    pub fn listen(self, on_show: impl Fn() + Send + 'static, on_quit: impl Fn() + Send + 'static) {
        std::thread::Builder::new()
            .name("prism-instance".into())
            .spawn(move || {
                let instance = self;
                let handles = [instance.show, instance.quit];
                loop {
                    let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                    match r.wrapping_sub(WAIT_OBJECT_0) {
                        0 => on_show(),
                        1 => {
                            on_quit();
                            return;
                        }
                        _ => return,
                    }
                }
            })
            .expect("instance thread");
    }
}
