#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod actions;
mod chart;
mod flags;
mod instance;
#[cfg(debug_assertions)]
mod shots;
mod view;

use prism_core::api::StartOptions;
use prism_core::events::{Event, EventSink};
use slint::ComponentHandle;
use std::path::PathBuf;
use std::sync::Arc;

struct UiSink;

impl EventSink for UiSink {
    fn emit(&self, event: Event) {
        let _ = slint::invoke_from_event_loop(move || view::apply(event));
    }
}

fn resource_dir() -> PathBuf {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default();
    if exe_dir.join("cores").is_dir() || !cfg!(debug_assertions) {
        exe_dir
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources")
    }
}

fn mono_font() -> &'static str {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    if windir.join(r"Fonts\CascadiaMono.ttf").exists() { "Cascadia Mono" } else { "Consolas" }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let _ = std::env::set_current_dir(std::env::temp_dir());
    if let Some(pid) = args.iter().find_map(|a| a.strip_prefix("--wait-pid=")).and_then(|p| p.parse().ok()) {
        prism_win::wait_for_pid(pid, 10_000);
    }
    let quit_requested = has("--quit");
    let instance = match instance::acquire(quit_requested) {
        instance::Start::Primary(instance) => instance,
        instance::Start::Secondary => return,
    };
    if quit_requested {
        return;
    }

    prism_ui::window::init_backend().expect("no window backend");
    let win = AppWindow::new().expect("main window");
    let tray = PrismTray::new().expect("tray icon");
    win.global::<Theme>().set_mono(mono_font().into());
    win.set_window_title(prism_win::profile().app_name.into());

    let state = prism_core::api::start(Arc::new(UiSink), StartOptions {
        data_dir: prism_win::profile().roaming_data(),
        resource_dir: resource_dir(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        enable_tun: has("--enable-tun"),
        connect_now: has("--connect"),
    });
    let history = state.speed_history.read().clone();
    view::init(win.clone_strong(), tray.clone_strong(), state.clone(), history);
    actions::bind(&win, &tray, &state);
    actions::check_updates_at_launch(&state);

    instance.listen(
        || {
            let _ = slint::invoke_from_event_loop(actions::show_window);
        },
        || {
            let _ = slint::invoke_from_event_loop(actions::quit);
        },
    );

    let _ = tray.show();
    if !has("--autostart") {
        actions::show_window();
        slint::Timer::single_shot(std::time::Duration::ZERO, || {
            if let Some(win) = view::with(|v| v.win.clone_strong()) {
                prism_ui::window::prepare(win.window());
            }
        });
    }
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("PRISM_DEV_SHOTS") {
        view::with(|v| v.dev_fake_connected());
        shots::start(win.clone_strong(), PathBuf::from(dir));
    }
    let _ = slint::run_event_loop_until_quit();

    let _ = win.hide();
    let _ = tray.hide();
    view::shutdown();
    prism_core::service::shutdown(&state);
}
