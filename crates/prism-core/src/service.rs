use crate::events::Event;
use crate::core::{CANCELLED, CoreProcess, free_port, port_available, wait_for_log, wait_for_port};
use crate::models::*;
use crate::net;
use crate::rules;
use crate::singbox_config::{self, TunInput};
use crate::state::{Cores, PROXY_BACKUP_FILE, Shared, direct_client};
use crate::storage::{load_json, save_json, write_atomic};
use crate::win::{self, PROXY_BYPASS, ProxySettings};
use crate::xray_config::{self, MainInput};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub const ELEVATION_REQUIRED: &str = "elevation-required";
pub const ELEVATION_TEXT: &str = "Для режима TUN нужны права администратора";

const SINGBOX_START_TIMEOUT: Duration = Duration::from_secs(60);
const TUN_ATTEMPTS: u32 = 3;
const TUN_RETRY_DELAY: Duration = Duration::from_secs(2);
const RECONNECT_DELAY: Duration = Duration::from_millis(300);

const TUN_SETUP_ERROR: &str = "configure tun interface: ";

fn explain_tun_error(e: String) -> String {
    match e.split_once(TUN_SETUP_ERROR) {
        Some((_, reason)) => format!(
            "Windows не смог настроить TUN-адаптер: {}. Перезагрузите компьютер и подключитесь снова",
            reason.trim_end_matches('.')
        ),
        None => e,
    }
}

pub fn ensure_geo(state: &Shared) {
    for name in ["geoip.dat", "geosite.dat"] {
        let dst = state.paths.geo.join(name);
        if !dst.exists() {
            let src = state.paths.bundled(&format!("geo/{name}"));
            if let Err(e) = std::fs::copy(&src, &dst) {
                state.logs.error(format!("не удалось скопировать {name}: {e}"));
            }
        }
    }
}

const WAITING_FOR_NETWORK: &str = "Ждём, пока появится сеть";
const IP_ATTEMPTS: u32 = 3;
const IP_RETRY_DELAY: Duration = Duration::from_secs(5);

async fn resolve_server(state: &Shared, server: &VlessConfig, session: u64, at_launch: bool) -> Result<String, String> {
    let limit = if at_launch { net::LAUNCH_WAIT } else { Duration::ZERO };
    let mut waited = false;
    let result = net::resolve_patiently(&server.address, server.port, limit, || !state.session_is(session), || {
        waited = true;
        state.logs.info("сети пока нет, жду подключения к интернету");
        state.update_conn(|c| c.note = Some(WAITING_FOR_NETWORK.into()));
    })
    .await;
    if waited && state.session_is(session) {
        state.update_conn(|c| c.note = None);
    }
    result
}

pub fn enable_proxy(state: &Shared, port: u16) -> Result<(), String> {
    if !win::profile().manages_system_proxy() {
        return Ok(());
    }
    let ours = format!("127.0.0.1:{port}");
    let backup_path = state.paths.file(PROXY_BACKUP_FILE);
    if !state.sysproxy_active.load(Ordering::SeqCst) && !backup_path.exists() {
        let mut current = win::read_proxy()?;
        if current.server == ours {
            current = ProxySettings::default();
        }
        save_json(&backup_path, &current)?;
    }
    win::write_proxy(&ProxySettings {
        enabled: true,
        server: ours,
        bypass: PROXY_BYPASS.into(),
        auto_config_url: String::new(),
    })?;
    state.sysproxy_active.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn restore_proxy(state: &Shared) {
    if !win::profile().manages_system_proxy() {
        return;
    }
    let backup_path = state.paths.file(PROXY_BACKUP_FILE);
    let was_active = state.sysproxy_active.swap(false, Ordering::SeqCst);
    if !was_active && !backup_path.exists() {
        return;
    }
    let backup: ProxySettings = load_json(&backup_path);
    match win::write_proxy(&backup) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup_path);
        }
        Err(e) => state.logs.error(e),
    }
}

async fn start_cores(
    state: &Shared,
    cores: &mut Cores,
    settings: &Settings,
    server: &Server,
    session: u64,
    at_launch: bool,
    tun_address: Option<String>,
) -> Result<(), String> {
    ensure_geo(state);
    if settings.tun && !state.elevated {
        return Err(ELEVATION_REQUIRED.into());
    }
    let listen = if settings.allow_lan { "0.0.0.0" } else { "127.0.0.1" };
    for port in [settings.socks_port, settings.http_port] {
        if !port_available(listen, port) {
            return Err(format!("Порт {port} занят другой программой — смените его в настройках"));
        }
    }

    let server_ip = match (settings.tun, tun_address) {
        (false, _) => None,
        (true, Some(ip)) => Some(ip),
        (true, None) => Some(resolve_server(state, &server.config, session, at_launch).await?),
    };
    let profile = state.routing.read().profile.clone();
    let geo = rules::GeoFiles::load(&state.paths.geo);
    let (proxy_rules, missing_proxy) = rules::available(&settings.proxy_rules, &geo);
    let (direct_rules, missing_direct) = rules::available(&settings.direct_rules, &geo);
    for entry in missing_proxy.iter().chain(&missing_direct) {
        state.logs.push("app", "warning", format!("в базе правил больше нет категории {entry}, правило пропущено"));
    }
    let metrics_port = free_port();
    let cfg = xray_config::build_main(&MainInput {
        server: &server.config,
        server_address: server_ip.as_deref(),
        mode: settings.routing_mode,
        profile: &profile,
        socks_port: settings.socks_port,
        http_port: settings.http_port,
        allow_lan: settings.allow_lan,
        metrics_port,
        log_level: &settings.log_level,
        ru_direct: settings.ru_direct,
        ru_cloud_ip: settings.ru_direct && xray_config::has_ru_cloud_ip(&state.paths.geo.join("geoip.dat")),
        proxy_rules: &proxy_rules,
        direct_rules: &direct_rules,
    });
    let cfg_path = state.paths.run.join("xray.json");
    write_atomic(&cfg_path, &serde_json::to_vec_pretty(&cfg).unwrap_or_default())?;

    let since = state.logs.last_id();
    let cfg_arg = cfg_path.to_string_lossy().to_string();
    let mut xray = CoreProcess::spawn(
        "xray",
        &state.paths.core_exe("xray.exe"),
        &["run", "-c", &cfg_arg],
        &[("XRAY_LOCATION_ASSET", &state.paths.geo)],
        &state.paths.run,
        &state.logs,
        state.job.as_ref(),
    )?;
    wait_for_port(&mut xray, settings.socks_port, Duration::from_secs(8), &state.logs, since).await?;
    cores.xray = Some(xray);
    cores.metrics_port = metrics_port;
    cores.socks_port = settings.socks_port;

    if settings.tun {
        let own_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "prism.exe".into());
        let bypass = vec!["xray.exe".to_string(), "sing-box.exe".to_string(), own_exe];
        let cfg = singbox_config::build(&TunInput {
            socks_port: settings.socks_port,
            server_ip: server_ip.as_deref(),
            stack: &settings.tun_stack,
            mtu: settings.tun_mtu,
            bypass_processes: &bypass,
            direct_apps: &settings.direct_apps,
        });
        if cores.singbox_config.as_ref() == Some(&cfg) && cores.singbox.as_mut().is_some_and(|p| p.is_running()) {
            return Ok(());
        }
        cores.stop_singbox();
        let sb_path = state.paths.run.join("singbox.json");
        write_atomic(&sb_path, &serde_json::to_vec_pretty(&cfg).unwrap_or_default())?;
        let sb_arg = sb_path.to_string_lossy().to_string();
        let run_dir = state.paths.run.to_string_lossy().to_string();
        let cancelled = || !state.session_is(session);
        let mut attempt = 1;
        loop {
            let since = state.logs.last_id();
            let mut sb = CoreProcess::spawn(
                "sing-box",
                &state.paths.core_exe("sing-box.exe"),
                &["run", "-c", &sb_arg, "-D", &run_dir, "--disable-color"],
                &[],
                &state.paths.run,
                &state.logs,
                state.job.as_ref(),
            )?;
            match wait_for_log(&mut sb, "sing-box started", SINGBOX_START_TIMEOUT, &state.logs, since, cancelled).await {
                Ok(()) => {
                    cores.singbox = Some(sb);
                    cores.singbox_config = Some(cfg);
                    cores.tun_server = server_ip.map(|ip| (server.config.clone(), ip));
                    break;
                }
                Err(e) if attempt < TUN_ATTEMPTS && e.contains(TUN_SETUP_ERROR) => {
                    drop(sb);
                    attempt += 1;
                    state.logs.info(format!("TUN-адаптер не настроился, попытка {attempt} из {TUN_ATTEMPTS}"));
                    tokio::time::sleep(TUN_RETRY_DELAY).await;
                    if cancelled() {
                        return Err(CANCELLED.into());
                    }
                }
                Err(e) => return Err(explain_tun_error(e)),
            }
        }
    }
    Ok(())
}

pub async fn connect(state: &Shared) -> Result<(), String> {
    connect_with(state, false).await
}

pub async fn connect_at_launch(state: &Shared) -> Result<(), String> {
    connect_with(state, true).await
}

async fn connect_with(state: &Shared, at_launch: bool) -> Result<(), String> {
    let mut cores = state.cores.lock().await;
    let settings = state.settings.read().clone();
    let Some(server) = state.current_server() else {
        let msg = "Добавьте подписку или сервер".to_string();
        state.update_conn(|c| {
            c.status = ConnStatus::Error;
            c.error = Some(msg.clone());
        });
        return Err(msg);
    };
    let session = state.next_session();
    let (was_connected, prev_since) = {
        let c = state.conn.read();
        (c.status == ConnStatus::Connected, c.since)
    };
    state.update_conn(|c| {
        c.status = ConnStatus::Connecting;
        c.server_id = Some(server.id.clone());
        c.mode = settings.routing_mode;
        c.tun = settings.tun;
        c.error = None;
        c.note = None;
        c.ip = None;
        c.country = None;
        c.ip_pending = false;
    });
    cores.stop_xray();
    let tun_address = if settings.tun { cores.tun_address(&server.config) } else { None };
    if tun_address.is_none() {
        cores.stop_singbox();
    }
    let started = Instant::now();

    let mut result = start_cores(state, &mut cores, &settings, &server, session, at_launch, tun_address).await;
    if result.as_ref().is_err_and(|e| e == CANCELLED) {
        cores.stop();
        return Ok(());
    }
    if result.is_ok() {
        result = if settings.tun {
            restore_proxy(state);
            Ok(())
        } else {
            enable_proxy(state, settings.http_port)
        };
    }
    if let Err(e) = result {
        cores.stop();
        restore_proxy(state);
        let shown = if e == ELEVATION_REQUIRED { ELEVATION_TEXT.to_string() } else { e.clone() };
        state.logs.error(&shown);
        state.update_conn(|c| {
            c.status = ConnStatus::Error;
            c.error = Some(shown);
            c.note = None;
            c.since = None;
        });
        return Err(e);
    }

    state.logs.info(format!(
        "подключено к «{}» за {} мс ({}, {})",
        display_name(&server.name),
        started.elapsed().as_millis(),
        if settings.routing_mode == RoutingMode::Smart { "Всё, кроме РФ" } else { "Глобальный" },
        if settings.tun { "TUN" } else { "системный прокси" },
    ));
    state.update_conn(|c| {
        c.status = ConnStatus::Connected;
        c.since = if was_connected { prev_since } else { Some(now_millis()) };
        c.ip_pending = true;
    });
    spawn_stats(state.clone(), session, cores.metrics_port);
    let st = state.clone();
    crate::rt::spawn(async move { check_ip(&st, session).await });
    Ok(())
}

pub async fn disconnect(state: &Shared) {
    state.next_session();
    state.update_conn(|c| c.status = ConnStatus::Disconnecting);
    let mut cores = state.cores.lock().await;
    restore_proxy(state);
    cores.stop();
    state.update_conn(|c| {
        c.status = ConnStatus::Disconnected;
        c.since = None;
        c.ip = None;
        c.country = None;
        c.ip_pending = false;
        c.error = None;
        c.note = None;
    });
    state.logs.info("отключено");
}

pub async fn toggle(state: &Shared) -> Result<(), String> {
    match state.status() {
        ConnStatus::Connected | ConnStatus::Connecting => {
            disconnect(state).await;
            Ok(())
        }
        _ => connect(state).await,
    }
}

fn is_active(state: &Shared) -> bool {
    matches!(state.status(), ConnStatus::Connected | ConnStatus::Connecting)
}

pub fn reconnect_if_connected(state: &Shared) {
    if !is_active(state) {
        return;
    }
    let seq = state.reconnect_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let st = state.clone();
    crate::rt::spawn(async move {
        tokio::time::sleep(RECONNECT_DELAY).await;
        if st.reconnect_seq.load(Ordering::SeqCst) == seq && is_active(&st) {
            let _ = connect(&st).await;
        }
    });
}

pub fn shutdown(state: &Shared) {
    state.next_session();
    restore_proxy(state);
    if let Ok(mut cores) = state.cores.try_lock() {
        cores.stop();
    }
}

#[derive(Clone, Debug)]
pub struct Traffic {
    pub up: f64,
    pub down: f64,
    pub total_up: u64,
    pub total_down: u64,
}

fn spawn_stats(state: Shared, session: u64, metrics_port: u16) {
    crate::rt::spawn(async move {
        let client = direct_client(Duration::from_secs(2));
        let url = format!("http://127.0.0.1:{metrics_port}/debug/vars");
        let mut prev: Option<(Instant, u64, u64)> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        while state.session_is(session) {
            tick.tick().await;
            let Ok(resp) = client.get(&url).send().await else { continue };
            let Ok(v) = resp.json::<Value>().await else { continue };
            let n = |tag: &str, dir: &str| v["stats"]["outbound"][tag][dir].as_u64().unwrap_or(0);
            let (pu, pd) = (n(xray_config::PROXY, "uplink"), n(xray_config::PROXY, "downlink"));
            let (du, dd) = (n(xray_config::DIRECT, "uplink"), n(xray_config::DIRECT, "downlink"));
            let (up_total, down_total) = (pu + du, pd + dd);
            let now = Instant::now();
            let (up, down) = match prev {
                Some((t, u, d)) => {
                    let dt = now.duration_since(t).as_secs_f64().max(0.001);
                    (up_total.saturating_sub(u) as f64 / dt, down_total.saturating_sub(d) as f64 / dt)
                }
                None => (0.0, 0.0),
            };
            prev = Some((now, up_total, down_total));
            if state.session_is(session) {
                state.emit(Event::Traffic(Traffic { up, down, total_up: up_total, total_down: down_total }));
            }
        }
    });
}

async fn trace(client: &reqwest::Client) -> Option<(Option<String>, Option<String>)> {
    let text = client.get("https://www.cloudflare.com/cdn-cgi/trace").send().await.ok()?.text().await.ok()?;
    let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()));
    Some((field("ip="), field("loc=")))
}

pub async fn check_ip(state: &Shared, session: u64) {
    if !state.session_is(session) {
        return;
    }
    state.update_conn(|c| c.ip_pending = true);
    let mut found = None;
    for attempt in 0..IP_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(IP_RETRY_DELAY).await;
        }
        if !state.session_is(session) {
            return;
        }
        let Some(client) = state.tunnel_client(Duration::from_secs(10)) else { break };
        found = trace(&client).await;
        if found.is_some() {
            break;
        }
    }
    if !state.session_is(session) {
        return;
    }
    if found.is_none() {
        state.logs.push("app", "warning", "не удалось определить внешний IP через туннель");
    }
    state.update_conn(|c| {
        c.ip_pending = false;
        if let Some((ip, country)) = found {
            c.ip = ip;
            c.country = country;
        }
    });
}

pub fn spawn_background_loops(state: Shared) {
    let logs_state = state.clone();
    crate::rt::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            logs_state.logs.flush(logs_state.events.as_ref());
        }
    });

    crate::rt::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if state.status() != ConnStatus::Connected {
                continue;
            }
            let Ok(mut cores) = state.cores.try_lock() else { continue };
            let Some(source) = cores.any_dead() else { continue };
            let detail = state.logs.last_error_since(0, source).map(|m| format!(": {m}")).unwrap_or_default();
            let msg = format!("{source} неожиданно завершился{detail}");
            state.next_session();
            restore_proxy(&state);
            cores.stop();
            state.logs.error(&msg);
            state.update_conn(|c| {
                c.status = ConnStatus::Error;
                c.error = Some(msg.clone());
                c.since = None;
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tun_setup_errors_are_explained() {
        let e = "sing-box завершился: start service: start inbound/tun[tun-in]: configure tun interface: \
                 A device attached to the system is not functioning.";
        assert_eq!(
            explain_tun_error(e.into()),
            "Windows не смог настроить TUN-адаптер: A device attached to the system is not functioning. \
             Перезагрузите компьютер и подключитесь снова"
        );
        assert_eq!(explain_tun_error("sing-box не запустился вовремя".into()), "sing-box не запустился вовремя");
    }
}
