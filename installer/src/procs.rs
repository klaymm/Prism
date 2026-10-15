use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub path: PathBuf,
}

fn image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

fn lower(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

pub fn running_in(dirs: &[PathBuf]) -> Vec<Proc> {
    let prefixes: Vec<String> = dirs
        .iter()
        .map(|d| {
            let mut s = lower(d);
            if !s.ends_with('\\') {
                s.push('\\');
            }
            s
        })
        .collect();
    let own = std::process::id();
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let pid = entry.th32ProcessID;
            if pid != own {
                if let Some(path) = image_path(pid) {
                    let p = lower(&path);
                    if prefixes.iter().any(|pre| p.starts_with(pre)) {
                        out.push(Proc { pid, path });
                    }
                }
            }
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    out
}

pub enum KillError {
    AccessDenied,
    Failed,
}

pub fn can_terminate(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
        true
    }
}

pub fn terminate(pid: u32) -> Result<(), KillError> {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return Err(if GetLastError() == ERROR_ACCESS_DENIED { KillError::AccessDenied } else { KillError::Failed });
        }
        let ok = TerminateProcess(h, 1);
        WaitForSingleObject(h, 5000);
        CloseHandle(h);
        if ok != 0 { Ok(()) } else { Err(KillError::Failed) }
    }
}

pub fn request_quit(procs: &[Proc]) -> bool {
    let mut asked = false;
    for p in procs {
        let is_app = p.path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("prism.exe"));
        if is_app {
            let profile = prism_win::profile().args();
            asked |= std::process::Command::new(&p.path)
                .arg("--quit")
                .args((!profile.is_empty()).then_some(profile))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .is_ok();
        }
    }
    asked
}

pub fn wait_gone(dirs: &[PathBuf], timeout: Duration) -> Vec<Proc> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = running_in(dirs);
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub enum CloseError {
    NeedsElevation,
    Stuck(String),
}

pub fn close_all(dirs: &[PathBuf]) -> Result<bool, CloseError> {
    let running = running_in(dirs);
    if running.is_empty() {
        return Ok(false);
    }
    let grace = if request_quit(&running) { Duration::from_secs(5) } else { Duration::ZERO };
    let left = wait_gone(dirs, grace);
    for p in &left {
        match terminate(p.pid) {
            Ok(()) => {}
            Err(KillError::AccessDenied) => return Err(CloseError::NeedsElevation),
            Err(KillError::Failed) => {}
        }
    }
    let left = wait_gone(dirs, Duration::from_secs(5));
    match left.first() {
        None => Ok(true),
        Some(p) => Err(CloseError::Stuck(p.path.display().to_string())),
    }
}

pub fn needs_elevation(dirs: &[PathBuf]) -> bool {
    running_in(dirs).iter().any(|p| !can_terminate(p.pid))
}
