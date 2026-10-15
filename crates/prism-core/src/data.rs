use crate::models::*;
use crate::routing::{self, RoutingState, parse_deeplink};
use crate::rules::{self, Target};
use crate::service::{self, reconnect_if_connected};
use crate::state::Shared;
use crate::subscription::{self, Fetched, apply_meta, host_title, merge_servers, parse_body};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ImportResult {
    pub added: usize,
    pub skipped: usize,
    pub name: Option<String>,
}

async fn fetch_subscription(state: &Shared, url: &str) -> Result<Fetched, String> {
    let ua = state.settings.read().user_agent.clone();
    match subscription::fetch(&state.direct, url, &ua).await {
        Ok(f) => Ok(f),
        Err(direct_err) => match state.tunnel_client(Duration::from_secs(30)) {
            Some(client) => subscription::fetch(&client, url, &ua).await.map_err(|_| direct_err),
            None => Err(direct_err),
        },
    }
}

async fn fetch_text(state: &Shared, url: &str) -> Result<String, String> {
    async fn get(client: &reqwest::Client, url: &str) -> Result<String, String> {
        let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        resp.text().await.map_err(|e| e.to_string())
    }
    match get(&state.direct, url).await {
        Ok(t) => Ok(t),
        Err(e) => match state.tunnel_client(Duration::from_secs(30)) {
            Some(client) => get(&client, url).await.map_err(|_| e),
            None => Err(e),
        },
    }
}

fn is_http_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && !s.contains(char::is_whitespace)
}

fn fix_selection(state: &Shared) {
    let selected = state.settings.read().selected_server.clone();
    let valid = selected.as_deref().is_some_and(|id| state.find_server(id).is_some());
    if !valid {
        let first = state.store.read().servers.first().map(|s| s.id.clone());
        state.settings.write().selected_server = first;
        state.save_settings();
    }
}

fn refresh_live_server(state: &Shared, before: Option<Server>) {
    let conn_server = state.conn.read().server_id.clone();
    let Some(id) = conn_server else { return };
    let now = state.find_server(&id);
    let changed = match (&before, &now) {
        (Some(b), Some(n)) => b.config != n.config,
        _ => true,
    };
    if changed && matches!(state.status(), ConnStatus::Connected | ConnStatus::Connecting) {
        if now.is_none() {
            let st = state.clone();
            crate::rt::spawn(async move {
                if st.current_server().is_some() {
                    let _ = service::connect(&st).await;
                } else {
                    service::disconnect(&st).await;
                }
            });
        } else {
            reconnect_if_connected(state);
        }
    }
}

pub async fn add_subscription(state: &Shared, url: &str) -> Result<ImportResult, String> {
    let url = url.trim();
    if !is_http_url(url) {
        return Err("Ссылка на подписку должна начинаться с https://".into());
    }
    let existing = state.store.read().subscriptions.iter().find(|s| s.url == url).map(|s| s.id.clone());
    if let Some(id) = existing {
        return update_subscription(state, &id).await;
    }
    let fetched = fetch_subscription(state, url).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut sub = Subscription { id: id.clone(), name: host_title(url), url: url.to_string(), ..Default::default() };
    apply_meta(&mut sub, fetched.meta);
    let result = ImportResult {
        added: fetched.parsed.servers.len(),
        skipped: fetched.parsed.skipped,
        name: Some(sub.name.clone()),
    };
    {
        let mut store = state.store.write();
        let servers = merge_servers(&[], &id, fetched.parsed.servers);
        store.subscriptions.push(sub);
        store.servers.extend(servers);
    }
    state.save_store();
    fix_selection(state);
    state.logs.info(format!("добавлена подписка «{}»: {} серверов", result.name.as_deref().unwrap_or(""), result.added));
    Ok(result)
}

pub async fn update_subscription(state: &Shared, id: &str) -> Result<ImportResult, String> {
    let url = state
        .store
        .read()
        .subscriptions
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.url.clone())
        .ok_or("подписка не найдена")?;
    let live_before = state.conn.read().server_id.clone().and_then(|sid| state.find_server(&sid));

    let fetched = match fetch_subscription(state, &url).await {
        Ok(f) => f,
        Err(e) => {
            if let Some(sub) = state.store.write().subscriptions.iter_mut().find(|s| s.id == id) {
                sub.last_error = Some(e.clone());
            }
            state.save_store();
            return Err(e);
        }
    };

    let result = {
        let mut store = state.store.write();
        let old: Vec<Server> = store.servers.iter().filter(|s| s.subscription_id.as_deref() == Some(id)).cloned().collect();
        let skipped = fetched.parsed.skipped;
        let fresh = merge_servers(&old, id, fetched.parsed.servers);
        let added = fresh.len();
        let insert_at = store
            .servers
            .iter()
            .position(|s| s.subscription_id.as_deref() == Some(id))
            .unwrap_or(store.servers.len());
        store.servers.retain(|s| s.subscription_id.as_deref() != Some(id));
        let insert_at = insert_at.min(store.servers.len());
        store.servers.splice(insert_at..insert_at, fresh);
        let sub = store.subscriptions.iter_mut().find(|s| s.id == id).ok_or("подписка не найдена")?;
        apply_meta(sub, fetched.meta);
        ImportResult { added, skipped, name: Some(sub.name.clone()) }
    };
    state.save_store();
    fix_selection(state);
    refresh_live_server(state, live_before);
    Ok(result)
}

pub async fn update_due_subscriptions(state: &Shared, force: bool) {
    let now = now_secs();
    let due: Vec<String> = state
        .store
        .read()
        .subscriptions
        .iter()
        .filter(|s| {
            let interval = i64::from(s.update_interval_hours.unwrap_or(12).max(1)) * 3600;
            force || s.updated_at.is_none_or(|t| now - t >= interval)
        })
        .map(|s| s.id.clone())
        .collect();
    for id in due {
        if let Err(e) = update_subscription(state, &id).await {
            state.logs.push("app", "warning", format!("обновление подписки: {e}"));
        }
    }
}

pub async fn import_text(state: &Shared, text: &str) -> Result<ImportResult, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Буфер обмена пуст".into());
    }
    if is_http_url(text) {
        return add_subscription(state, text).await;
    }
    let parsed = parse_body(text);
    if parsed.servers.is_empty() {
        return Err("Не удалось распознать ссылку. Поддерживаются подписки (https://…) и ссылки vless://".into());
    }
    let skipped = parsed.skipped;
    let added = {
        let mut store = state.store.write();
        let mut added = 0;
        for (name, link, config) in parsed.servers {
            if store.servers.iter().any(|s| s.link == link) {
                continue;
            }
            store.servers.push(Server {
                id: uuid::Uuid::new_v4().to_string(),
                subscription_id: None,
                name,
                link,
                config,
                latency: None,
            });
            added += 1;
        }
        added
    };
    state.save_store();
    fix_selection(state);
    Ok(ImportResult { added, skipped, name: None })
}

pub fn remove_subscription(state: &Shared, id: &str) {
    let live_before = state.conn.read().server_id.clone().and_then(|sid| state.find_server(&sid));
    {
        let mut store = state.store.write();
        store.subscriptions.retain(|s| s.id != id);
        store.servers.retain(|s| s.subscription_id.as_deref() != Some(id));
    }
    state.save_store();
    fix_selection(state);
    refresh_live_server(state, live_before);
}

pub fn rename_subscription(state: &Shared, id: &str, name: &str) {
    if let Some(sub) = state.store.write().subscriptions.iter_mut().find(|s| s.id == id) {
        sub.name = name.trim().to_string();
    }
    state.save_store();
}

pub fn remove_server(state: &Shared, id: &str) {
    let live_before = state.conn.read().server_id.clone().and_then(|sid| state.find_server(&sid));
    state.store.write().servers.retain(|s| s.id != id);
    state.save_store();
    fix_selection(state);
    refresh_live_server(state, live_before);
}

pub fn select_server(state: &Shared, id: &str) -> Result<(), String> {
    if state.find_server(id).is_none() {
        return Err("сервер не найден".into());
    }
    state.settings.write().selected_server = Some(id.to_string());
    state.save_settings();
    let live = state.conn.read().server_id.clone();
    if live.as_deref() != Some(id) {
        reconnect_if_connected(state);
    }
    Ok(())
}

pub fn set_mode(state: &Shared, mode: RoutingMode) {
    if state.settings.read().routing_mode == mode {
        return;
    }
    state.settings.write().routing_mode = mode;
    state.save_settings();
    state.update_conn(|c| c.mode = mode);
    reconnect_if_connected(state);
}

const TUN_DISABLED: &str = "В dev-профиле режим TUN отключён";

pub fn set_tun(state: &Shared, enabled: bool) -> Result<(), String> {
    if enabled && !crate::win::profile().allows_tun() {
        return Err(TUN_DISABLED.into());
    }
    if enabled && !state.elevated {
        return Err("elevation-required".into());
    }
    if state.settings.read().tun == enabled {
        return Ok(());
    }
    state.settings.write().tun = enabled;
    state.save_settings();
    state.update_conn(|c| c.tun = enabled);
    apply_autostart(state);
    reconnect_if_connected(state);
    Ok(())
}

pub fn update_settings(state: &Shared, patch: serde_json::Value) -> Result<Settings, String> {
    let old = state.settings.read().clone();
    let mut merged = serde_json::to_value(&old).map_err(|e| e.to_string())?;
    if let (Some(dst), Some(src)) = (merged.as_object_mut(), patch.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let new: Settings = serde_json::from_value(merged).map_err(|e| format!("некорректные настройки: {e}"))?;
    if new.tun && !old.tun && !crate::win::profile().allows_tun() {
        return Err(TUN_DISABLED.into());
    }
    if new.tun && !old.tun && !state.elevated {
        return Err("elevation-required".into());
    }
    if new.socks_port == new.http_port || new.socks_port < 1024 || new.http_port < 1024 {
        return Err("Порты должны быть разными и не меньше 1024".into());
    }
    *state.settings.write() = new.clone();
    state.save_settings();

    if new.autostart != old.autostart || new.tun != old.tun {
        apply_autostart(state);
    }
    if new.routing_url != old.routing_url {
        let st = state.clone();
        crate::rt::spawn(async move {
            let _ = refresh_routing(&st).await;
        });
    }
    let needs_restart = new.routing_mode != old.routing_mode
        || new.ru_direct != old.ru_direct
        || new.proxy_rules != old.proxy_rules
        || new.direct_rules != old.direct_rules
        || new.direct_apps != old.direct_apps
        || new.tun != old.tun
        || new.socks_port != old.socks_port
        || new.http_port != old.http_port
        || new.allow_lan != old.allow_lan
        || new.tun_stack != old.tun_stack
        || new.tun_mtu != old.tun_mtu
        || new.log_level != old.log_level;
    if needs_restart {
        state.update_conn(|c| {
            c.mode = new.routing_mode;
            c.tun = new.tun;
        });
        reconnect_if_connected(state);
    }
    Ok(new)
}

#[derive(Debug, Default)]
pub struct RulesChange {
    pub added: usize,
    pub moved: Vec<String>,
    pub rejected: Vec<String>,
}

pub fn add_rules(state: &Shared, target: Target, text: &str) -> RulesChange {
    let parsed = rules::parse(text, &rules::GeoFiles::load(&state.paths.geo));
    let mut change = RulesChange { rejected: parsed.rejected, ..Default::default() };
    {
        let mut guard = state.settings.write();
        let s = &mut *guard;
        let (list, other) = match target {
            Target::Proxy => (&mut s.proxy_rules, &mut s.direct_rules),
            Target::Direct => (&mut s.direct_rules, &mut s.proxy_rules),
        };
        for entry in parsed.entries {
            if let Some(i) = other.iter().position(|e| *e == entry) {
                other.remove(i);
                change.moved.push(entry.clone());
            }
            if !list.contains(&entry) {
                list.push(entry);
                change.added += 1;
            }
        }
    }
    if change.added > 0 {
        state.save_settings();
        reconnect_if_connected(state);
    }
    change
}

pub fn remove_rule(state: &Shared, target: Target, entry: &str) {
    {
        let mut s = state.settings.write();
        let list = match target {
            Target::Proxy => &mut s.proxy_rules,
            Target::Direct => &mut s.direct_rules,
        };
        list.retain(|e| e != entry);
    }
    state.save_settings();
    reconnect_if_connected(state);
}

pub fn add_apps(state: &Shared, text: &str) -> RulesChange {
    let parsed = rules::parse_apps(text);
    let mut change = RulesChange { rejected: parsed.rejected, ..Default::default() };
    {
        let mut s = state.settings.write();
        for app in parsed.entries {
            if !s.direct_apps.iter().any(|a| a.to_lowercase() == app.to_lowercase()) {
                s.direct_apps.push(app);
                change.added += 1;
            }
        }
    }
    if change.added > 0 {
        state.save_settings();
        reconnect_if_connected(state);
    }
    change
}

pub fn remove_app(state: &Shared, name: &str) {
    state.settings.write().direct_apps.retain(|a| a != name);
    state.save_settings();
    reconnect_if_connected(state);
}

fn hidden(program: &str, args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(program)
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn schtasks(args: &[&str]) -> bool {
    hidden("schtasks", args)
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn run_key() -> std::io::Result<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(RUN_KEY).map(|(k, _)| k)
}

fn set_run_entry(name: &str, command: Option<&str>) -> std::io::Result<()> {
    let key = run_key()?;
    match command {
        Some(cmd) => key.set_value(name, &cmd.to_string()),
        None => match key.delete_value(name) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn current_user() -> String {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    match (var("USERDOMAIN"), var("USERNAME")) {
        (_, user) if user.is_empty() => String::new(),
        (domain, user) if domain.is_empty() => user,
        (domain, user) => format!("{domain}\\{user}"),
    }
}

fn logon_task_xml(user: &str, exe: &str, args: &str) -> String {
    let user_id = if user.is_empty() { String::new() } else { format!("<UserId>{}</UserId>", xml_escape(user)) };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      {user_id}
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {user_id}
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        exe = xml_escape(exe),
        args = xml_escape(args),
    )
}

fn create_logon_task(state: &Shared, name: &str, exe: &str, args: &str) -> bool {
    let path = state.paths.run.join("autostart-task.xml");
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(logon_task_xml(&current_user(), exe, args).encode_utf16().flat_map(u16::to_le_bytes));
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let created = schtasks(&["/Create", "/F", "/TN", name, "/XML", &path.to_string_lossy()]);
    let _ = std::fs::remove_file(&path);
    created
}

pub fn apply_autostart(state: &Shared) {
    let task = crate::win::profile().app_name;
    let (autostart, tun) = {
        let s = state.settings.read();
        (s.autostart, s.tun)
    };
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let args = format!("--autostart {}", crate::win::profile().args()).trim_end().to_string();
    let command = format!("\"{exe}\" {args}");
    if !autostart {
        let _ = set_run_entry(task, None);
        if state.elevated {
            schtasks(&["/Delete", "/F", "/TN", task]);
        }
        return;
    }
    if tun && state.elevated {
        if create_logon_task(state, task, &exe, &args) {
            let _ = set_run_entry(task, None);
            return;
        }
        state.logs.push("app", "warning", "не удалось создать задачу автозапуска с правами администратора");
    }
    if let Err(e) = set_run_entry(task, Some(&command)) {
        state.logs.error(format!("автозапуск: {e}"));
    }
}

pub fn init_routing(state: &Shared) {
    service::ensure_geo(state);
    if !state.routing.read().profile.name.is_empty() || !state.routing.read().profile.direct_sites.is_empty() {
        return;
    }
    let raw = std::fs::read_to_string(state.paths.bundled("routing/DEFAULT.DEEPLINK")).unwrap_or_default();
    match parse_deeplink(&raw) {
        Ok(profile) => {
            let mut r = state.routing.write();
            r.source_url = state.settings.read().routing_url.clone();
            r.geoip_url = profile.geoip_url.clone();
            r.geosite_url = profile.geosite_url.clone();
            r.profile = profile;
        }
        Err(e) => state.logs.error(format!("встроенный профиль роутинга: {e}")),
    }
    state.save_routing();
}

pub async fn refresh_routing(state: &Shared) -> Result<RoutingState, String> {
    let url = state.settings.read().routing_url.clone();
    let result: Result<bool, String> = async {
        let raw = fetch_text(state, &url).await.map_err(|e| format!("профиль роутинга: {e}"))?;
        let profile = parse_deeplink(&raw)?;
        let current = state.routing.read().clone();
        let mut changed = current.profile != profile;

        for (file, url, known) in [
            ("geoip.dat", &profile.geoip_url, &current.geoip_url),
            ("geosite.dat", &profile.geosite_url, &current.geosite_url),
        ] {
            let dest = state.paths.geo.join(file);
            if url.is_empty() || (url == known && dest.exists()) {
                continue;
            }
            let res = routing::download_to(&state.direct, url, &dest).await;
            let res = match (res, state.tunnel_client(Duration::from_secs(60))) {
                (Err(_), Some(client)) => routing::download_to(&client, url, &dest).await,
                (r, _) => r,
            };
            res?;
            changed = true;
        }

        let mut r = state.routing.write();
        r.source_url = url.clone();
        r.fetched_at = Some(now_secs());
        r.geoip_url = profile.geoip_url.clone();
        r.geosite_url = profile.geosite_url.clone();
        r.profile = profile;
        r.last_error = None;
        Ok(changed)
    }
    .await;

    match result {
        Ok(changed) => {
            state.save_routing();
            if changed {
                state.logs.info("профиль роутинга обновлён");
                reconnect_if_connected(state);
            }
            Ok(state.routing.read().clone())
        }
        Err(e) => {
            state.routing.write().last_error = Some(e.clone());
            state.save_routing();
            state.logs.push("app", "warning", e.clone());
            Err(e)
        }
    }
}

pub fn spawn_schedulers(state: Shared) {
    crate::rt::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let routing_url = state.settings.read().routing_url.clone();
        let host = url::Url::parse(&routing_url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        crate::net::wait_for_network(&host, crate::net::LAUNCH_WAIT).await;
        let mut tick = tokio::time::interval(Duration::from_secs(30 * 60));
        loop {
            tick.tick().await;
            let stale = state.routing.read().fetched_at.is_none_or(|t| now_secs() - t > 6 * 3600);
            if stale {
                let _ = refresh_routing(&state).await;
            }
            update_due_subscriptions(&state, false).await;
            if crate::updater::installed() && crate::updater::check_due(&state) {
                let _ = crate::updater::check(&state).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logon_task_keeps_running() {
        let xml = logon_task_xml(r"PC\Аня", r"C:\Apps\R&D\prism.exe", "--autostart --profile=dev");
        for setting in [
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<Priority>5</Priority>",
            "<RunLevel>HighestAvailable</RunLevel>",
        ] {
            assert!(xml.contains(setting), "{setting} missing");
        }
        assert_eq!(xml.matches(r"<UserId>PC\Аня</UserId>").count(), 2, "trigger and principal name the user");
        assert!(xml.contains(r#"<Command>"C:\Apps\R&amp;D\prism.exe"</Command>"#));
        assert!(xml.contains("<Arguments>--autostart --profile=dev</Arguments>"));
        assert!(!logon_task_xml("", "prism.exe", "--autostart").contains("<UserId>"));
        std::fs::write(crate::test_output_dir("prism-config-check").join("autostart-task.xml"), xml).unwrap();
    }

    struct NoEvents;

    impl crate::events::EventSink for NoEvents {
        fn emit(&self, _: crate::events::Event) {}
    }

    fn test_state(name: &str) -> Shared {
        let dir = crate::test_output_dir(name);
        let geo = dir.join("geo");
        std::fs::create_dir_all(&geo).unwrap();
        std::fs::write(geo.join("geosite.dat"), crate::rules::tests::geo_file(&["STEAM"])).unwrap();
        std::fs::write(geo.join("geoip.dat"), crate::rules::tests::geo_file(&["DIRECT"])).unwrap();
        let _ = std::fs::remove_file(dir.join(crate::state::SETTINGS_FILE));
        let paths = crate::storage::Paths::new(dir.clone(), dir);
        std::sync::Arc::new(crate::state::AppState::new(std::sync::Arc::new(NoEvents), paths))
    }

    #[test]
    fn rules_move_between_lists() {
        let st = test_state("prism-rules-state");
        let change = add_rules(&st, Target::Direct, "example.com, geosite:steam, bad..name");
        assert_eq!(change.added, 2);
        assert_eq!(change.rejected, ["bad..name"]);

        let change = add_rules(&st, Target::Proxy, "https://example.com/page");
        assert_eq!(change.moved, ["example.com"]);
        assert_eq!(st.settings.read().proxy_rules, ["example.com"]);
        assert_eq!(st.settings.read().direct_rules, ["geosite:steam"]);

        let again = add_rules(&st, Target::Proxy, "EXAMPLE.com");
        assert_eq!((again.added, again.moved.len()), (0, 0));

        remove_rule(&st, Target::Proxy, "example.com");
        assert!(st.settings.read().proxy_rules.is_empty());

        let change = add_apps(&st, "C:\\Games\\Doomsday.exe, readme.txt");
        assert_eq!((change.added, change.rejected.as_slice()), (1, ["readme.txt".to_string()].as_slice()));
        assert_eq!(add_apps(&st, "doomsday").added, 0, "names compare without case");
        remove_app(&st, "Doomsday.exe");
        assert!(st.settings.read().direct_apps.is_empty());
    }
}
