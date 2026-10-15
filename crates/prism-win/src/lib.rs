mod install;
mod profile;
pub use install::{AUTO_INSTALL_ARG, InstallOptions};
pub use profile::{DEV_ARG, Profile, profile};

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE};
use windows_sys::Win32::Networking::WinInet::*;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxySettings {
    pub enabled: bool,
    pub server: String,
    pub bypass: String,
    pub auto_config_url: String,
}

pub const PROXY_BYPASS: &str = "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>";

pub fn read_proxy() -> Result<ProxySettings, String> {
    let mut options = [
        INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_FLAGS, ..Default::default() },
        INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_SERVER, ..Default::default() },
        INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_BYPASS, ..Default::default() },
        INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_AUTOCONFIG_URL, ..Default::default() },
    ];
    let mut list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: std::ptr::null_mut(),
        dwOptionCount: options.len() as u32,
        dwOptionError: 0,
        pOptions: options.as_mut_ptr(),
    };
    let mut size = size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32;
    let ok = unsafe {
        InternetQueryOptionW(
            std::ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            &mut list as *mut _ as *mut c_void,
            &mut size,
        )
    };
    if ok == 0 {
        return Err("не удалось прочитать настройки прокси Windows".into());
    }
    unsafe {
        let flags = options[0].Value.dwValue;
        let take = |i: usize| {
            let p = options[i].Value.pszValue;
            let s = from_wide(p);
            if !p.is_null() {
                GlobalFree(p as *mut c_void);
            }
            s
        };
        let server = take(1);
        let bypass = take(2);
        let pac = take(3);
        Ok(ProxySettings {
            enabled: flags & PROXY_TYPE_PROXY != 0,
            server,
            bypass,
            auto_config_url: if flags & PROXY_TYPE_AUTO_PROXY_URL != 0 { pac } else { String::new() },
        })
    }
}

pub fn write_proxy(p: &ProxySettings) -> Result<(), String> {
    let mut flags = PROXY_TYPE_DIRECT;
    if p.enabled {
        flags |= PROXY_TYPE_PROXY;
    }
    if !p.auto_config_url.is_empty() {
        flags |= PROXY_TYPE_AUTO_PROXY_URL;
    }
    let mut server = wide(&p.server);
    let mut bypass = wide(&p.bypass);
    let mut pac = wide(&p.auto_config_url);
    let mut options = [
        INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_FLAGS, Value: INTERNET_PER_CONN_OPTIONW_0 { dwValue: flags } },
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_PROXY_SERVER,
            Value: INTERNET_PER_CONN_OPTIONW_0 { pszValue: server.as_mut_ptr() },
        },
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_PROXY_BYPASS,
            Value: INTERNET_PER_CONN_OPTIONW_0 { pszValue: bypass.as_mut_ptr() },
        },
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_AUTOCONFIG_URL,
            Value: INTERNET_PER_CONN_OPTIONW_0 { pszValue: pac.as_mut_ptr() },
        },
    ];
    let list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: std::ptr::null_mut(),
        dwOptionCount: options.len() as u32,
        dwOptionError: 0,
        pOptions: options.as_mut_ptr(),
    };
    unsafe {
        let ok = InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            &list as *const _ as *const c_void,
            size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        );
        if ok == 0 {
            return Err("не удалось изменить настройки прокси Windows".into());
        }
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
    Ok(())
}

pub struct KillOnCloseJob(HANDLE);

unsafe impl Send for KillOnCloseJob {}
unsafe impl Sync for KillOnCloseJob {}

impl KillOnCloseJob {
    pub fn new() -> Option<Self> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self(job))
        }
    }

    pub fn assign(&self, child: &std::process::Child) {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE);
        }
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

pub fn relaunch_elevated(extra_args: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args = format!("--wait-pid={} {} {}", std::process::id(), profile().args(), extra_args);
    let exe_w = wide(&exe.to_string_lossy());
    let args_w = wide(args.trim());
    let verb = wide("runas");
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            exe_w.as_ptr(),
            args_w.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err("запуск от имени администратора отменён".into());
    }
    Ok(())
}

pub fn wait_for_pid(pid: u32, timeout_ms: u32) {
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, timeout_ms);
            CloseHandle(h);
        }
    }
}

pub fn restore_proxy_from_backup(path: &std::path::Path) -> Result<bool, String> {
    let Ok(bytes) = std::fs::read(path) else { return Ok(false) };
    let backup: ProxySettings = serde_json::from_slice(&bytes).unwrap_or_default();
    write_proxy(&backup)?;
    let _ = std::fs::remove_file(path);
    Ok(true)
}
