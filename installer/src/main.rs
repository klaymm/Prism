#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod detect;
mod log;
mod ops;
mod payload;
mod procs;
#[cfg(debug_assertions)]
mod shots;

use ops::NEEDS_ELEVATION;
use payload::Payload;
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter().find_map(|a| a.strip_prefix(&format!("{key}=")).map(String::from))
}

fn relaunch_from_temp(args: &[String], target: &Path) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let temp = std::env::temp_dir().join(format!("prism-uninstall-{}.exe", std::process::id()));
    if std::fs::copy(&exe, &temp).is_err() {
        return false;
    }
    let mut cmd = std::process::Command::new(&temp);
    cmd.arg("--uninstall").arg("--from-temp").arg(format!("--dir={}", target.display()));
    if prism_win::profile().dev {
        cmd.arg(prism_win::DEV_ARG);
    }
    if args.iter().any(|a| a == "--silent") {
        cmd.arg("--silent");
    }
    cmd.spawn().is_ok()
}

fn split_path(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.split('\\').collect();
    if parts.len() <= 3 {
        return (path.to_string(), String::new());
    }
    let tail = format!("\\{}", parts[parts.len() - 2..].join("\\"));
    (path[..path.len() - tail.len()].to_string(), tail)
}

fn to_ui(o: &prism_win::InstallOptions) -> InstallOptions {
    InstallOptions { dir: o.dir.as_str().into(), desktop: o.desktop, autostart: o.autostart, launch: o.launch }
}

fn from_ui(o: &InstallOptions, base: &prism_win::InstallOptions) -> prism_win::InstallOptions {
    prism_win::InstallOptions {
        dir: o.dir.to_string(),
        desktop: o.desktop,
        autostart: o.autostart,
        launch: o.launch,
        ..base.clone()
    }
}

fn existing_note(existing: &[detect::Existing], version: &str) -> String {
    let app = detect::app_name();
    match existing.first() {
        None => String::new(),
        Some(e) if e.version == version => {
            format!("{app} {} уже установлен — переустановим начисто, настройки сохранятся.", e.version)
        }
        Some(e) => format!("Установлен {app} {} — обновим до {version}, настройки сохранятся.", e.version),
    }
}

struct Context {
    uninstalling: bool,
    target: PathBuf,
    payload: Option<Arc<Payload>>,
    exe: std::sync::Mutex<Option<PathBuf>>,
    auto: Option<prism_win::InstallOptions>,
}

fn set_options(win: &SetupWindow, opts: InstallOptions) {
    let (head, tail) = split_path(&opts.dir);
    let setup = win.global::<Setup>();
    setup.set_dir_head(head.into());
    setup.set_dir_tail(tail.into());
    setup.set_options(opts);
}

fn fail(win: &SetupWindow, error: String) {
    log::write(format!("ошибка: {error}"));
    let setup = win.global::<Setup>();
    setup.set_error(error.into());
    setup.set_screen(Screen::Error);
}

fn elevate(win: &SetupWindow, ctx: &Context, options: Option<prism_win::InstallOptions>) {
    let mut args = match options {
        Some(o) => o.to_arg(),
        None => format!("--uninstall --from-temp \"--dir={}\"", ctx.target.display()),
    };
    if let Ok(src) = std::env::var("PRISM_PAYLOAD") {
        args.push_str(&format!(" \"--payload={src}\""));
    }
    match prism_win::relaunch_elevated(&args) {
        Ok(()) => {
            let _ = slint::quit_event_loop();
        }
        Err(e) => fail(
            win,
            format!("Чтобы закрыть клиент, запущенный от имени администратора, нужно подтвердить запрос Windows. {e}"),
        ),
    }
}

fn reporter(weak: slint::Weak<SetupWindow>) -> impl FnMut(&'static str, f64) {
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut last_step = "";
    move |step, fraction| {
        if step != last_step || last.elapsed() >= Duration::from_millis(60) || fraction >= 1.0 {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    let setup = win.global::<Setup>();
                    setup.set_step(step.into());
                    setup.set_progress(fraction as f32);
                }
            });
            last = Instant::now();
            last_step = step;
        }
    }
}

fn install(win: &SetupWindow, ctx: Arc<Context>) {
    let setup = win.global::<Setup>();
    let opts = from_ui(&setup.get_options(), &ctx.auto.clone().unwrap_or_default());
    setup.set_step("extract".into());
    setup.set_progress(0.0);
    setup.set_screen(Screen::Progress);
    let Some(payload) = ctx.payload.clone() else {
        fail(win, "В установщике нет файлов приложения".into());
        return;
    };
    let weak = win.as_weak();
    std::thread::spawn(move || {
        let mut report = reporter(weak.clone());
        let result = ops::install(&payload, &opts, |step, f| report(step.key(), f));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = weak.upgrade() else { return };
            match result {
                Ok(exe) => {
                    if opts.launch {
                        let _ = ops::launch(&exe, &opts.args);
                    }
                    if opts.update {
                        let _ = slint::quit_event_loop();
                        return;
                    }
                    *ctx.exe.lock().unwrap() = Some(exe);
                    win.global::<Setup>().set_screen(Screen::Done);
                }
                Err(e) if e == NEEDS_ELEVATION => elevate(&win, &ctx, Some(opts)),
                Err(e) => fail(&win, e),
            }
        });
    });
}

fn uninstall(win: &SetupWindow, ctx: Arc<Context>) {
    let setup = win.global::<Setup>();
    let remove_data = setup.get_remove_data();
    setup.set_step("uninstall".into());
    setup.set_progress(0.0);
    setup.set_screen(Screen::Progress);
    let weak = win.as_weak();
    std::thread::spawn(move || {
        let mut report = reporter(weak.clone());
        let result = ops::uninstall(&ctx.target, remove_data, |f| report("uninstall", f));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = weak.upgrade() else { return };
            match result {
                Ok(()) => win.global::<Setup>().set_screen(Screen::Done),
                Err(e) if e == NEEDS_ELEVATION => elevate(&win, &ctx, None),
                Err(e) => fail(&win, e),
            }
        });
    });
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if let Some(pid) = arg_value(&args, "--wait-pid").and_then(|p| p.parse().ok()) {
        prism_win::wait_for_pid(pid, 10_000);
    }
    if let Some(src) = arg_value(&args, "--payload") {
        unsafe { std::env::set_var("PRISM_PAYLOAD", src) };
    }

    let named_uninstaller = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.eq_ignore_ascii_case("uninstall")))
        .unwrap_or(false);
    let uninstalling = has("--uninstall") || named_uninstaller;
    if uninstalling {
        let _ = std::env::set_current_dir(std::env::temp_dir());
    }
    let target = arg_value(&args, "--dir").map(PathBuf::from).unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default()
    });
    let mut cleanup = Vec::new();
    if uninstalling {
        if !has("--from-temp") {
            if relaunch_from_temp(&args, &target) {
                return;
            }
        } else if let Ok(exe) = std::env::current_exe() {
            cleanup.push(exe);
        }
        if has("--silent") {
            if let Err(e) = ops::uninstall(&target, false, |_| {}) {
                log::write(format!("тихое удаление: {e}"));
            }
            ops::schedule_cleanup(&cleanup);
            return;
        }
    }

    let payload = if uninstalling { None } else { payload::locate().map(Arc::new) };
    let payload_broken = !uninstalling && payload.is_none();
    let auto_install = arg_value(&args, prism_win::AUTO_INSTALL_ARG).and_then(|h| prism_win::InstallOptions::from_arg(&h));

    prism_ui::window::init_backend().expect("no window backend");
    let win = SetupWindow::new().expect("setup window");
    win.global::<Motion>().set_reduced(!prism_ui::window::animations_enabled());
    let app = detect::app_name();
    let setup = win.global::<Setup>();
    let title = if uninstalling {
        format!("Удаление {app}")
    } else if auto_install.as_ref().is_some_and(|o| o.update) {
        format!("Обновление {app}")
    } else {
        format!("Установка {app}")
    };
    win.set_window_title(title.into());

    let manifest = payload.as_ref().map(|p| &p.manifest);
    let version = manifest.map(|m| m.version.clone()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    let existing = if uninstalling { Vec::new() } else { detect::find() };
    let dirs: Vec<PathBuf> =
        if uninstalling { vec![target.clone()] } else { existing.iter().map(|e| e.location.clone()).collect() };
    setup.set_uninstalling(uninstalling);
    setup.set_app_name(app.into());
    setup.set_version(version.as_str().into());
    setup.set_install_size(prism_ui::format::bytes_with(manifest.map_or(0, |m| m.total_size()) as f64, 0).into());
    setup.set_existing_note(existing_note(&existing, &version).into());
    setup.set_upgrade(!existing.is_empty());
    setup.set_needs_elevation(!prism_win::is_elevated() && procs::needs_elevation(&dirs));
    setup.set_payload_broken(payload_broken);
    let defaults = prism_win::InstallOptions {
        dir: ops::default_install_dir().display().to_string(),
        desktop: true,
        launch: true,
        ..Default::default()
    };
    set_options(&win, to_ui(auto_install.as_ref().unwrap_or(&defaults)));

    let ctx = Arc::new(Context { uninstalling, target, payload, exe: std::sync::Mutex::new(None), auto: auto_install.clone() });

    let weak = win.as_weak();
    let c = ctx.clone();
    setup.on_start_install(move || {
        let Some(win) = weak.upgrade() else { return };
        if win.global::<Setup>().get_needs_elevation() {
            let opts = from_ui(&win.global::<Setup>().get_options(), &c.auto.clone().unwrap_or_default());
            elevate(&win, &c, Some(opts));
        } else {
            install(&win, c.clone());
        }
    });
    let weak = win.as_weak();
    let c = ctx.clone();
    setup.on_start_uninstall(move || {
        let Some(win) = weak.upgrade() else { return };
        if win.global::<Setup>().get_needs_elevation() {
            elevate(&win, &c, None);
        } else {
            uninstall(&win, c.clone());
        }
    });
    let weak = win.as_weak();
    setup.on_pick_dir(move || {
        let Some(win) = weak.upgrade() else { return };
        let mut opts = win.global::<Setup>().get_options();
        let current = PathBuf::from(opts.dir.as_str());
        let start = current.parent().filter(|p| p.is_dir()).map(Path::to_path_buf).unwrap_or(current);
        if let Some(picked) = rfd::FileDialog::new().set_directory(start).pick_folder() {
            opts.dir = ops::normalize_dir(&picked.display().to_string()).display().to_string().into();
            set_options(&win, opts);
        }
    });
    let c = ctx.clone();
    setup.on_launch_app(move || {
        if let Some(exe) = c.exe.lock().unwrap().clone() {
            let _ = ops::launch(&exe, &[]);
        }
        let _ = slint::quit_event_loop();
    });
    setup.on_open_log(|| {
        let _ = std::process::Command::new("notepad").arg(log::path()).spawn();
    });
    setup.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    let weak = win.as_weak();
    setup.on_minimize(move || {
        if let Some(win) = weak.upgrade() {
            prism_ui::window::minimize(win.window());
        }
    });
    let weak = win.as_weak();
    setup.on_caption_pressed(move || {
        if let Some(win) = weak.upgrade() {
            prism_ui::window::drag(win.window());
        }
    });
    let weak = win.as_weak();
    win.window().on_close_requested(move || {
        let busy = weak.upgrade().is_some_and(|w| w.global::<Setup>().get_screen() == Screen::Progress);
        if busy {
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        }
    });

    setup.set_screen(if payload_broken {
        setup.set_error("Установщик повреждён: не найдены файлы приложения. Скачайте его заново.".into());
        Screen::Error
    } else if ctx.uninstalling {
        Screen::Confirm
    } else {
        Screen::Welcome
    });
    if auto_install.is_some() && !payload_broken {
        install(&win, ctx.clone());
    }

    let _ = win.show();
    let weak = win.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        if let Some(win) = weak.upgrade() {
            prism_ui::window::prepare(win.window());
        }
    });
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("PRISM_SETUP_SHOTS") {
        shots::start(win.clone_strong(), PathBuf::from(dir));
    }
    let _ = slint::run_event_loop_until_quit();
    let _ = win.hide();
    ops::schedule_cleanup(&cleanup);
}
