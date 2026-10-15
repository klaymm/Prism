use crate::detect::{self, Existing, UNINSTALL_KEY, app_name};
use crate::log;
use crate::payload::{self, Payload};
use crate::procs::{self, CloseError};
use prism_win::InstallOptions;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const NEEDS_ELEVATION: &str = "needs-elevation";

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_default()
}

fn data_id() -> &'static str {
    prism_win::profile().data_id
}

pub fn default_install_dir() -> PathBuf {
    env_dir("LOCALAPPDATA").join("Programs").join(app_name())
}

pub fn normalize_dir(picked: &str) -> PathBuf {
    let p = PathBuf::from(picked.trim());
    if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(app_name())) { p } else { p.join(app_name()) }
}

fn start_menu_dir() -> PathBuf {
    env_dir("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs")
}

fn shortcut_paths(name: &str) -> Vec<PathBuf> {
    let mut v = vec![start_menu_dir().join(format!("{name}.lnk"))];
    if let Some(desktop) = dirs::desktop_dir() {
        v.push(desktop.join(format!("{name}.lnk")));
    }
    v
}

fn roaming_data(id: &str) -> PathBuf {
    env_dir("APPDATA").join(id)
}

fn local_data(id: &str) -> PathBuf {
    env_dir("LOCALAPPDATA").join(id)
}

fn hidden(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn remove_dir_retry(dir: &Path) -> Result<(), String> {
    const ATTEMPTS: u32 = 60;
    for attempt in 0..ATTEMPTS {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if attempt == ATTEMPTS - 1 => return Err(format!("не удалось удалить {}: {e}", dir.display())),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    Ok(())
}

fn rename_retry(from: &Path, to: &Path) -> Result<(), String> {
    const ATTEMPTS: u32 = 60;
    for attempt in 0..ATTEMPTS {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt == ATTEMPTS - 1 => {
                return Err(format!("не удалось переместить {}: {e}", from.display()));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    Ok(())
}

fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    dir.with_file_name(format!("{name}.{suffix}"))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

fn replace_dir(staging: &Path, dir: &Path) -> Result<(), String> {
    let old = sibling(dir, "old");
    remove_dir_retry(&old)?;
    let had_old = dir.exists();
    if had_old {
        rename_retry(dir, &old)?;
    }
    if let Err(e) = std::fs::rename(staging, dir) {
        if had_old {
            let _ = std::fs::rename(&old, dir);
        }
        return Err(format!("не удалось поставить новую версию в {}: {e}", dir.display()));
    }
    if had_old && remove_dir_retry(&old).is_err() {
        schedule_cleanup(&[old]);
    }
    Ok(())
}

fn move_into(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("не удалось создать {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            move_into(&src, &dst)?;
        } else {
            let _ = std::fs::remove_file(&dst);
            std::fs::rename(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
    }
    std::fs::remove_dir_all(from).map_err(|e| e.to_string())
}

fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

fn remove_run_value(name: &str) {
    if let Ok(run) = hkcu().open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE) {
        let _ = run.delete_value(name);
    }
}

fn restore_proxy() {
    if !prism_win::profile().manages_system_proxy() {
        return;
    }
    let backup = roaming_data(data_id()).join("sysproxy-backup.json");
    match prism_win::restore_proxy_from_backup(&backup) {
        Ok(true) => log::write(format!("прокси Windows восстановлен из {}", backup.display())),
        Ok(false) => {}
        Err(e) => log::write(format!("не удалось восстановить прокси: {e}")),
    }
}

pub fn close_app(dirs: &[PathBuf]) -> Result<(), String> {
    match procs::close_all(dirs) {
        Ok(closed) => {
            if closed {
                log::write("запущенный клиент закрыт");
            }
            restore_proxy();
            Ok(())
        }
        Err(CloseError::NeedsElevation) => Err(NEEDS_ELEVATION.into()),
        Err(CloseError::Stuck(p)) => Err(format!("Не удалось закрыть {p}. Закройте программу и повторите.")),
    }
}

fn remove_prism_files(location: &Path) -> Result<(), String> {
    remove_dir_retry(location)?;
    for lnk in shortcut_paths(app_name()) {
        let _ = std::fs::remove_file(lnk);
    }
    if let Ok(root) = hkcu().open_subkey_with_flags(UNINSTALL_KEY, winreg::enums::KEY_ALL_ACCESS) {
        let _ = root.delete_subkey_all(app_name());
    }
    let _ = std::fs::remove_dir_all(local_data(data_id()).join("EBWebView"));
    Ok(())
}

pub fn remove_previous(existing: &[Existing]) -> Result<(), String> {
    for e in existing {
        log::write(format!("удаление {} {} из {}", app_name(), e.version, e.location.display()));
        remove_prism_files(&e.location)?;
    }
    Ok(())
}

fn create_shortcut(target: &Path, lnk: &Path) -> Result<(), String> {
    let mut link = mslnk::ShellLink::new(target).map_err(|e| e.to_string())?;
    let args = prism_win::profile().args();
    if !args.is_empty() {
        link.set_arguments(Some(args.to_string()));
    }
    link.set_working_dir(target.parent().map(|p| p.to_string_lossy().to_string()));
    link.set_icon_location(Some(target.to_string_lossy().to_string()));
    link.set_name(Some("Prism — VPN-клиент VLESS + Reality".into()));
    if let Some(parent) = lnk.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    link.create_lnk(lnk).map_err(|e| format!("ярлык {}: {e}", lnk.display()))
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

fn register(dir: &Path, exe: &Path, version: &str) -> Result<(), String> {
    let (key, _) = hkcu()
        .create_subkey(format!(r"{UNINSTALL_KEY}\{}", app_name()))
        .map_err(|e| format!("реестр: {e}"))?;
    let uninstaller = dir.join("uninstall.exe");
    let quoted = |p: &Path| format!("\"{}\"", p.display());
    let set = |name: &str, value: String| key.set_value(name, &value).map_err(|e| format!("реестр: {e}"));
    set("DisplayName", app_name().into())?;
    set("DisplayVersion", version.into())?;
    set("Publisher", "klaymm".into())?;
    set("DisplayIcon", format!("{},0", exe.display()))?;
    set("InstallLocation", dir.display().to_string())?;
    let dev = if prism_win::profile().dev { format!(" {}", prism_win::DEV_ARG) } else { String::new() };
    set("UninstallString", format!("{} --uninstall{dev}", quoted(&uninstaller)))?;
    set("QuietUninstallString", format!("{} --uninstall --silent{dev}", quoted(&uninstaller)))?;
    set("URLInfoAbout", "https://github.com/klaymm/Prism".into())?;
    let size_kb = (dir_size(dir) / 1024) as u32;
    key.set_value("EstimatedSize", &size_kb).map_err(|e| format!("реестр: {e}"))?;
    key.set_value("NoModify", &1u32).map_err(|e| format!("реестр: {e}"))?;
    key.set_value("NoRepair", &1u32).map_err(|e| format!("реестр: {e}"))?;
    Ok(())
}

fn enable_autostart(exe: &Path) -> Result<(), String> {
    let (run, _) = hkcu().create_subkey(RUN_KEY).map_err(|e| format!("автозапуск: {e}"))?;
    let command = format!("\"{}\" --autostart {}", exe.display(), prism_win::profile().args());
    run.set_value(app_name(), &command.trim_end()).map_err(|e| format!("автозапуск: {e}"))?;
    let data = roaming_data(data_id());
    let _ = std::fs::create_dir_all(&data);
    let path = data.join("settings.json");
    let mut settings: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    settings["autostart"] = serde_json::Value::Bool(true);
    std::fs::write(&path, serde_json::to_vec_pretty(&settings).unwrap_or_default()).map_err(|e| e.to_string())
}

#[derive(Clone, Copy)]
pub enum Step {
    Extract,
    Close,
    Replace,
    Shortcuts,
    Register,
}

impl Step {
    pub fn key(self) -> &'static str {
        match self {
            Step::Extract => "extract",
            Step::Close => "close",
            Step::Replace => "replace",
            Step::Shortcuts => "shortcuts",
            Step::Register => "register",
        }
    }

    fn start(self) -> f64 {
        match self {
            Step::Extract => 0.0,
            Step::Close => 0.8,
            Step::Replace => 0.86,
            Step::Shortcuts => 0.92,
            Step::Register => 0.96,
        }
    }

    fn end(self) -> f64 {
        match self {
            Step::Extract => 0.8,
            Step::Close => 0.86,
            Step::Replace => 0.92,
            Step::Shortcuts => 0.96,
            Step::Register => 1.0,
        }
    }
}

pub fn install(payload: &Payload, opts: &InstallOptions, mut report: impl FnMut(Step, f64)) -> Result<PathBuf, String> {
    let dir = normalize_dir(&opts.dir);
    let exe = dir.join(&payload.manifest.exe);
    log::write(format!("установка {} {} в {}", payload.manifest.name, payload.manifest.version, dir.display()));

    report(Step::Extract, Step::Extract.start());
    let staging = sibling(&dir, "new");
    remove_dir_retry(&staging)?;
    std::fs::create_dir_all(&staging).map_err(|e| format!("не удалось создать {}: {e}", staging.display()))?;
    let total = payload.manifest.total_size().max(1) as f64;
    let span = Step::Extract.end() - Step::Extract.start();
    payload::extract(payload, &staging, |done| report(Step::Extract, Step::Extract.start() + span * done as f64 / total))?;
    payload::write_bare_exe(payload, &staging.join("uninstall.exe"))?;

    report(Step::Close, Step::Close.start());
    let existing = detect::find();
    let mut dirs: Vec<PathBuf> = existing.iter().map(|e| e.location.clone()).collect();
    dirs.push(dir.clone());
    close_app(&dirs)?;

    report(Step::Replace, Step::Replace.start());
    let installed_here = existing.iter().any(|e| same_dir(&e.location, &dir));
    if installed_here || !dir.exists() || dir.join(&payload.manifest.exe).exists() {
        replace_dir(&staging, &dir)?;
    } else {
        move_into(&staging, &dir)?;
    }
    let elsewhere: Vec<Existing> = existing.into_iter().filter(|e| !same_dir(&e.location, &dir)).collect();
    remove_previous(&elsewhere)?;

    report(Step::Shortcuts, Step::Shortcuts.start());
    let shortcuts = shortcut_paths(app_name());
    create_shortcut(&exe, &shortcuts[0])?;
    let desktop = if opts.update { shortcuts.get(1).is_some_and(|p| p.exists()) } else { opts.desktop };
    if desktop {
        if let Some(desktop) = shortcuts.get(1) {
            create_shortcut(&exe, desktop)?;
        }
    }

    report(Step::Register, Step::Register.start());
    register(&dir, &exe, &payload.manifest.version)?;
    if opts.autostart {
        enable_autostart(&exe)?;
    }
    report(Step::Register, 1.0);
    log::write("установка завершена");
    Ok(exe)
}

pub fn uninstall(dir: &Path, remove_data: bool, mut report: impl FnMut(f64)) -> Result<(), String> {
    log::write(format!("удаление {} из {}", app_name(), dir.display()));
    report(0.05);
    close_app(&[dir.to_path_buf()])?;
    report(0.3);
    remove_run_value(app_name());
    hidden("schtasks", &["/Delete", "/F", "/TN", app_name()]);
    remove_prism_files(dir)?;
    report(0.8);
    if remove_data {
        for d in [roaming_data(data_id()), local_data(data_id())] {
            let _ = remove_dir_retry(&d);
        }
    }
    report(1.0);
    log::write("удаление завершено");
    Ok(())
}

pub fn launch(exe: &Path, extra: &[String]) -> Result<(), String> {
    let args = prism_win::profile().args();
    std::process::Command::new(exe)
        .args((!args.is_empty()).then_some(args))
        .args(extra)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("не удалось запустить Prism: {e}"))
}

pub fn schedule_cleanup(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let targets = paths
        .iter()
        .map(|p| {
            if p.is_dir() {
                format!("rmdir /s /q \"{}\" 2>nul", p.display())
            } else {
                format!("del /f /q \"{}\" 2>nul", p.display())
            }
        })
        .collect::<Vec<_>>()
        .join(" & ");
    let script = [2, 3, 5]
        .iter()
        .map(|secs| format!("ping 127.0.0.1 -n {secs} >nul & {targets}"))
        .collect::<Vec<_>>()
        .join(" & ");
    let _ = std::process::Command::new("cmd").arg("/c").raw_arg(&script).creation_flags(CREATE_NO_WINDOW).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("prism-ops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn new_version_replaces_the_old_folder() {
        let root = temp_dir("replace");
        let (dir, staging) = (root.join("Prism"), root.join("Prism.new"));
        std::fs::create_dir_all(dir.join("cores")).unwrap();
        std::fs::write(dir.join("cores").join("stale.exe"), "old").unwrap();
        std::fs::create_dir_all(staging.join("cores")).unwrap();
        std::fs::write(staging.join("prism.exe"), "new").unwrap();
        replace_dir(&staging, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("prism.exe")).unwrap(), "new");
        assert!(!dir.join("cores").join("stale.exe").exists(), "nothing of the old version stays");
        assert!(!staging.exists() && !root.join("Prism.old").exists());

        let err = replace_dir(&root.join("missing.new"), &dir).unwrap_err();
        assert!(err.contains("новую версию"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.join("prism.exe")).unwrap(), "new", "a failed swap keeps the installed version");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn foreign_folder_keeps_its_files() {
        let root = temp_dir("merge");
        let (dir, staging) = (root.join("Prism"), root.join("Prism.new"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "mine").unwrap();
        std::fs::write(dir.join("prism.exe"), "old").unwrap();
        std::fs::create_dir_all(staging.join("geo")).unwrap();
        std::fs::write(staging.join("prism.exe"), "new").unwrap();
        std::fs::write(staging.join("geo").join("geoip.dat"), "x").unwrap();
        move_into(&staging, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(dir.join("prism.exe")).unwrap(), "new");
        assert!(dir.join("geo").join("geoip.dat").exists());
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folders_compare_without_case_and_trailing_slash() {
        assert!(same_dir(Path::new(r"C:\Users\A\Programs\Prism\"), Path::new(r"c:\users\a\programs\prism")));
        assert_eq!(sibling(Path::new(r"C:\P\Prism Dev"), "new"), PathBuf::from(r"C:\P\Prism Dev.new"));
    }

    #[test]
    fn picked_folder_gets_app_subfolder() {
        assert_eq!(normalize_dir(r"D:\Apps"), PathBuf::from(r"D:\Apps\Prism"));
        assert_eq!(normalize_dir(r"D:\Apps\Prism"), PathBuf::from(r"D:\Apps\Prism"));
        assert_eq!(normalize_dir(r"D:\Apps\prism"), PathBuf::from(r"D:\Apps\prism"));
    }
}
