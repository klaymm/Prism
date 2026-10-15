use crate::view::{self, LIVE_TARGET, ToastKind, View, import_message};
use crate::{App, AppWindow, Page, PrismTray};
use prism_core::models::{ConnStatus, LatencyKind, RoutingMode};
use prism_core::state::Shared;
use prism_core::rules::{self, Target};
use prism_core::updater::{self, Release, UpdateStatus};
use prism_core::{api, data, latency, rt, service, speedtest};
use slint::ComponentHandle;
use std::future::Future;
use std::sync::atomic::Ordering;

fn spawn<T: Send + 'static>(
    fut: impl Future<Output = T> + Send + 'static,
    done: impl FnOnce(&mut View, T) + Send + 'static,
) {
    rt::spawn(async move {
        let out = fut.await;
        let _ = slint::invoke_from_event_loop(move || {
            view::with(|v| done(v, out));
        });
    });
}

fn window() -> Option<AppWindow> {
    view::with(|v| v.win.clone_strong())
}

pub fn show_window() {
    if let Some(win) = window() {
        prism_ui::window::present(win.window());
        prism_ui::window::prepare(win.window());
    }
}

pub fn hide_window() {
    if let Some(win) = window() {
        win.window().dispatch_event(slint::platform::WindowEvent::PointerExited);
        let _ = win.hide();
    }
}

pub fn quit() {
    let _ = slint::quit_event_loop();
}

pub fn close_window() {
    if view::with(|v| v.settings.close_to_tray).unwrap_or(false) {
        hide_window();
    } else {
        quit();
    }
}

fn clipboard_text() -> Result<String, String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|_| "Не удалось прочитать буфер обмена".to_string())
}

fn copy_text(v: &mut View, text: &str, message: &str) {
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        Ok(()) => v.toast(ToastKind::Success, message),
        Err(e) => v.toast(ToastKind::Error, format!("Не удалось скопировать: {e}")),
    }
}

fn shell_open(target: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, target) = (wide("open"), wide(target));
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn mode_of(value: &str) -> RoutingMode {
    if value == "global" { RoutingMode::Global } else { RoutingMode::Smart }
}

fn set_tun(state: &Shared, enabled: bool, from_tray: bool) {
    if let Err(e) = data::set_tun(state, enabled) {
        if from_tray {
            show_window();
        }
        view::with(|v| v.fail(e));
    }
}

fn connect_here(state: &Shared, id: &str) {
    if let Err(e) = data::select_server(state, id) {
        view::with(|v| v.fail(e));
        return;
    }
    if !view::with(|v| v.is_active()).unwrap_or(false) {
        let st = state.clone();
        spawn(async move { service::connect(&st).await }, |v, r| {
            if let Err(e) = r {
                v.fail(e);
            }
        });
    }
}

fn ping(state: &Shared, kind: LatencyKind, ids: Vec<String>) {
    let started = view::with(|v| {
        if v.pinging {
            return false;
        }
        v.pinging = true;
        v.push_servers();
        true
    });
    if started != Some(true) {
        return;
    }
    let st = state.clone();
    spawn(async move { latency::ping(&st, Some(ids), kind).await }, |v, r| {
        v.pinging = false;
        v.push_servers();
        if let Err(e) = r {
            v.fail(e);
        }
    });
}

fn refresh_subscription(state: &Shared, id: String, detailed: bool) {
    view::with(|v| v.set_sub_busy(&id, true));
    let st = state.clone();
    let key = id.clone();
    spawn(async move { data::update_subscription(&st, &id).await }, move |v, r| {
        v.set_sub_busy(&key, false);
        match r {
            Ok(r) if detailed => v.toast(ToastKind::Success, import_message(&r)),
            Ok(_) => v.toast(ToastKind::Success, "Подписка обновлена"),
            Err(e) => v.fail(e),
        }
    });
}

fn import(state: &Shared, text: String, from_dialog: bool) {
    if text.trim().is_empty() {
        return;
    }
    if from_dialog {
        view::with(|v| {
            v.app().set_import_busy(true);
            v.app().set_import_error("".into());
        });
    }
    let st = state.clone();
    spawn(async move { data::import_text(&st, &text).await }, move |v, r| {
        let app = v.app();
        app.set_import_busy(false);
        match r {
            Ok(r) => {
                if from_dialog {
                    app.set_import_open(false);
                    app.set_import_text("".into());
                }
                v.toast(ToastKind::Success, import_message(&r));
            }
            Err(e) if from_dialog => app.set_import_error(e.into()),
            Err(e) => v.fail(e),
        }
    });
}

fn target_of(list: &str) -> Target {
    if list == "proxy" { Target::Proxy } else { Target::Direct }
}

fn add_rules(state: &Shared, list: &str, text: &str) -> String {
    let target = target_of(list);
    let apps = list == "apps";
    let change = if apps { data::add_apps(state, text) } else { data::add_rules(state, target, text) };
    view::with(|v| {
        if !change.rejected.is_empty() {
            let what = if apps {
                "Нужно имя файла программы, например game.exe"
            } else {
                "Не похоже на домен, IP-адрес, подсеть или категорию из базы"
            };
            v.toast(ToastKind::Error, format!("{what}: {}", change.rejected.join(", ")));
        } else if !change.moved.is_empty() {
            let from = if target == Target::Proxy { "«Напрямую»" } else { "«Через VPN»" };
            let moved: Vec<String> = change.moved.iter().map(|e| rules::display(e)).collect();
            v.toast(ToastKind::Success, format!("Перенесено из списка {from}: {}", moved.join(", ")));
        } else if change.added == 0 && !text.trim().is_empty() {
            v.toast(ToastKind::Success, "Уже в списке");
        }
    });
    change.rejected.join(if apps { ", " } else { " " })
}

fn pick_app(state: &Shared) {
    let picked = rfd::FileDialog::new()
        .set_title("Программа, которая пойдёт мимо VPN")
        .add_filter("Программы", &["exe"])
        .pick_file();
    if let Some(name) = picked.as_deref().and_then(|p| p.file_name()) {
        add_rules(state, "apps", &name.to_string_lossy());
    }
}

fn relaunch_args(v: &View, state: &Shared) -> Vec<String> {
    let mut args = Vec::new();
    if !v.win.window().is_visible() {
        args.push("--autostart".to_string());
    }
    if matches!(state.status(), ConnStatus::Connected | ConnStatus::Connecting) {
        args.push("--connect".to_string());
    }
    args
}

fn install_update(v: &mut View, state: &Shared, release: Release) {
    let args = relaunch_args(v, state);
    let st = state.clone();
    spawn(
        async move {
            let setup = updater::download(&st, &release).await?;
            updater::start_setup(&st, &setup, &release, args)
        },
        |v, r| match r {
            Ok(()) => v.toast(ToastKind::Success, "Ставим обновление, Prism перезапустится сам"),
            Err(e) => v.fail(e),
        },
    );
}

pub fn check_updates_at_launch(state: &Shared) {
    let st = state.clone();
    spawn(async move { updater::check_at_launch(&st).await }, {
        let st = state.clone();
        move |v, release| {
            if let Some(release) = release {
                install_update(v, &st, release);
            }
        }
    });
}

fn setting_patch(key: &str, value: &str) -> Option<serde_json::Value> {
    let v = value.trim();
    let val = match key {
        "theme" | "logLevel" | "tunStack" => serde_json::json!(v),
        "userAgent" | "routingUrl" if !v.is_empty() => serde_json::json!(v),
        "autostart" | "connectOnLaunch" | "closeToTray" | "allowLan" | "ruDirect" | "autoUpdate" => {
            serde_json::json!(v == "true")
        }
        "socksPort" | "httpPort" => serde_json::json!(v.parse::<u16>().ok()?),
        "tunMtu" => serde_json::json!(v.parse::<u32>().ok().filter(|n| (1280..=9000).contains(n))?),
        _ => return None,
    };
    Some(serde_json::json!({ key: val }))
}

pub fn bind(win: &AppWindow, tray: &PrismTray, state: &Shared) {
    let app = win.global::<App>();

    {
        use slint::winit_030::{WinitWindowAccessor, EventResult, winit};
        let weak = win.as_weak();
        win.window().on_winit_window_event(move |_, event| {
            if let winit::event::WindowEvent::Resized(_) = event {
                if let Some(win) = weak.upgrade() {
                    let maximized = win.window().is_maximized();
                    win.global::<App>().set_maximized(maximized);
                }
            }
            EventResult::Propagate
        });
    }
    win.window().on_close_requested(|| {
        slint::Timer::single_shot(std::time::Duration::ZERO, close_window);
        slint::CloseRequestResponse::KeepWindowShown
    });

    app.on_minimize(|| {
        if let Some(win) = window() {
            prism_ui::window::minimize(win.window());
        }
    });
    app.on_toggle_maximize(|| {
        if let Some(win) = window() {
            prism_ui::window::toggle_maximized(win.window());
        }
    });
    app.on_caption_pressed(|| {
        if let Some(win) = window() {
            prism_ui::window::caption_pressed(win.window());
        }
    });
    app.on_close_window(close_window);
    app.on_quit(quit);
    app.on_navigate(|page| {
        view::with(|v| v.navigate(page));
    });
    app.on_dismiss_toast(|id| {
        view::with(|v| v.dismiss_toast(id));
    });
    app.on_chart_hover(|fraction, _| view::chart_hover(fraction));
    app.on_chart_size(|w, h| {
        view::with(|v| v.chart_resized(w, h));
    });

    let st = state.clone();
    app.on_toggle_connection(move || {
        let st = st.clone();
        spawn(async move { service::toggle(&st).await }, |v, r| {
            if let Err(e) = r {
                v.fail(e);
            }
        });
    });
    let st = state.clone();
    app.on_set_mode(move |mode| data::set_mode(&st, mode_of(&mode)));
    let st = state.clone();
    app.on_set_tun(move |enabled| set_tun(&st, enabled, false));
    let st = state.clone();
    app.on_check_ip(move || {
        let st = st.clone();
        let session = st.session.load(Ordering::SeqCst);
        rt::spawn(async move { service::check_ip(&st, session).await });
    });
    let st = state.clone();
    app.on_restart_as_admin(move || {
        view::with(|v| v.app().set_elevation_busy(true));
        match api::restart_as_admin(&st) {
            Ok(()) => quit(),
            Err(e) => {
                view::with(|v| {
                    v.app().set_elevation_busy(false);
                    v.fail(e);
                });
            }
        }
    });

    // Subscriptions
    app.on_open_import(|| {
        view::with(|v| {
            let app = v.app();
            app.set_import_text("".into());
            app.set_import_error("".into());
            app.set_import_open(true);
        });
    });
    let st = state.clone();
    app.on_import_submit(move |text| import(&st, text.to_string(), true));
    let st = state.clone();
    app.on_import_paste(move || match clipboard_text() {
        Ok(text) => {
            let text = text.trim().to_string();
            view::with(|v| v.app().set_import_text(text.as_str().into()));
            import(&st, text, true);
        }
        Err(e) => {
            view::with(|v| v.app().set_import_error(e.into()));
        }
    });
    let st = state.clone();
    app.on_paste_anywhere(move || {
        let blocked = view::with(|v| v.app().get_import_open() || v.app().get_elevation_open()).unwrap_or(true);
        if blocked {
            return;
        }
        if let Ok(text) = clipboard_text() {
            import(&st, text.trim().to_string(), false);
        }
    });
    let st = state.clone();
    app.on_refresh_subscription(move |id| refresh_subscription(&st, id.to_string(), false));
    let st = state.clone();
    app.on_subscription_action(move |id, action| {
        let sub = st.store.read().subscriptions.iter().find(|s| s.id == id.as_str()).cloned();
        let Some(sub) = sub else { return };
        match action.as_str() {
            "refresh" => refresh_subscription(&st, sub.id, true),
            "copy" => {
                view::with(|v| copy_text(v, &sub.url, "Ссылка скопирована"));
            }
            "support" => {
                if let Some(url) = &sub.support_url {
                    shell_open(url);
                }
            }
            _ => {}
        }
    });
    let st = state.clone();
    app.on_rename_subscription(move |id, name| {
        let name = name.trim();
        if !name.is_empty() {
            data::rename_subscription(&st, &id, name);
        }
    });
    let st = state.clone();
    app.on_remove_subscription(move |id| {
        data::remove_subscription(&st, &id);
        view::with(|v| v.toast(ToastKind::Success, "Подписка удалена"));
    });
    app.on_servers_filter(|tab, query, sort| {
        view::with(|v| v.set_server_filter(tab.into(), query.into(), sort.into()));
    });
    let st = state.clone();
    app.on_select_server(move |id| {
        if let Err(e) = data::select_server(&st, &id) {
            view::with(|v| v.fail(e));
        }
    });
    let st = state.clone();
    app.on_connect_server(move |id| connect_here(&st, &id));
    let st = state.clone();
    app.on_server_action(move |id, action| match action.as_str() {
        "connect" => connect_here(&st, &id),
        "ping" => ping(&st, LatencyKind::Real, vec![id.to_string()]),
        "speed" => {
            view::with(|v| {
                v.set_speed_target(id.to_string());
                v.navigate(Page::Speed);
            });
        }
        "copy" => {
            let link = st.store.read().servers.iter().find(|s| s.id == id.as_str()).map(|s| s.link.clone());
            if let Some(link) = link {
                view::with(|v| copy_text(v, &link, "Ссылка скопирована"));
            }
        }
        "remove" => data::remove_server(&st, &id),
        _ => {}
    });
    let st = state.clone();
    app.on_ping(move |kind| {
        let ids = view::with(|v| v.visible_servers.clone()).unwrap_or_default();
        let kind = if kind == "tcp" { LatencyKind::Tcp } else { LatencyKind::Real };
        ping(&st, kind, ids);
    });

    // Routing
    let st = state.clone();
    app.on_refresh_routing(move || {
        view::with(|v| v.app().set_routing_busy(true));
        let st = st.clone();
        spawn(async move { data::refresh_routing(&st).await }, |v, r| {
            v.app().set_routing_busy(false);
            match r {
                Ok(_) => v.toast(ToastKind::Success, "Правила роутинга обновлены"),
                Err(e) => v.fail(e),
            }
        });
    });

    let st = state.clone();
    app.on_add_rules(move |list, text| add_rules(&st, &list, &text).into());
    let st = state.clone();
    app.on_remove_rule(move |list, entry| {
        if list == "apps" {
            data::remove_app(&st, &entry);
        } else {
            data::remove_rule(&st, target_of(&list), &entry);
        }
    });
    let st = state.clone();
    app.on_pick_app(move || pick_app(&st));

    // Speed test
    app.on_speed_select(|target| {
        view::with(|v| v.set_speed_target(target.into()));
    });
    let st = state.clone();
    app.on_speed_start(move || {
        let target = view::with(|v| {
            if v.speed_running {
                return None;
            }
            let target = v.app().get_speed_target().to_string();
            v.speed_started();
            Some(target)
        })
        .flatten();
        let Some(target) = target else { return };
        let server = (target != LIVE_TARGET).then_some(target);
        let st = st.clone();
        spawn(async move { speedtest::run(&st, server).await }, |v, r| v.speed_finished(r));
    });
    let st = state.clone();
    app.on_speed_cancel(move || st.speedtest_cancel.store(true, Ordering::SeqCst));

    // Logs
    app.on_logs_filter(|level, source, query| {
        view::with(|v| v.set_log_filter(level.into(), source.into(), query.into()));
    });
    app.on_copy_logs(|| {
        view::with(|v| {
            let text = v.visible_log_text();
            copy_text(v, &text, "Логи скопированы");
        });
    });
    app.on_copy_log(|id| {
        view::with(|v| {
            if let Some(line) = v.log_text(id) {
                copy_text(v, &line, "Строка скопирована");
            }
        });
    });
    let st = state.clone();
    app.on_clear_logs(move || {
        st.logs.clear();
        view::with(|v| v.clear_logs());
    });

    // Settings
    let st = state.clone();
    app.on_set_setting(move |key, value| {
        let Some(patch) = setting_patch(&key, &value) else { return };
        match data::update_settings(&st, patch) {
            Ok(_) => {}
            Err(e) => {
                view::with(|v| v.fail(e));
            }
        }
    });
    let st = state.clone();
    app.on_open_data_dir(move || shell_open(&st.paths.data.display().to_string()));
    let st = state.clone();
    app.on_check_update(move || {
        let st = st.clone();
        rt::spawn(async move {
            let _ = updater::check(&st).await;
        });
    });
    let st = state.clone();
    app.on_install_update(move || {
        let release = match &*st.update.read() {
            UpdateStatus::Available(r) => Some(r.clone()),
            _ => None,
        };
        if let Some(release) = release {
            view::with(|v| install_update(v, &st, release));
        }
    });
    let st = state.clone();
    app.on_open_release_page(move || {
        let url = match &*st.update.read() {
            UpdateStatus::Available(r) => r.page_url.clone(),
            _ => String::new(),
        };
        if !url.is_empty() {
            shell_open(&url);
        }
    });

    // Tray
    tray.on_open(show_window);
    tray.on_quit(quit);
    let st = state.clone();
    tray.on_toggle(move || {
        let st = st.clone();
        spawn(async move { service::toggle(&st).await }, |v, r| {
            if let Err(e) = r {
                v.fail(e);
            }
        });
    });
    let st = state.clone();
    tray.on_set_mode(move |mode| data::set_mode(&st, mode_of(&mode)));
    let st = state.clone();
    tray.on_select_server(move |id| {
        let _ = data::select_server(&st, &id);
    });
    let st = state.clone();
    tray.on_toggle_tun(move || {
        let enabled = !st.settings.read().tun;
        set_tun(&st, enabled, true);
    });
}
