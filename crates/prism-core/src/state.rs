use crate::core::CoreProcess;
use crate::events::{Event, EventSink};
use crate::logs::LogBuffer;
use crate::models::*;
use crate::routing::RoutingState;
use crate::storage::{Paths, load_json, save_json};
use crate::updater::UpdateStatus;
use crate::win::KillOnCloseJob;
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

pub const SETTINGS_FILE: &str = "settings.json";
pub const SERVERS_FILE: &str = "servers.json";
pub const ROUTING_FILE: &str = "routing.json";
pub const SPEED_FILE: &str = "speedtests.json";
pub const PROXY_BACKUP_FILE: &str = "sysproxy-backup.json";

#[derive(Default)]
pub struct Cores {
    pub xray: Option<CoreProcess>,
    pub singbox: Option<CoreProcess>,
    pub singbox_config: Option<serde_json::Value>,
    pub tun_server: Option<(VlessConfig, String)>,
    pub metrics_port: u16,
    pub socks_port: u16,
}

impl Cores {
    pub fn stop(&mut self) {
        self.stop_singbox();
        self.stop_xray();
    }

    pub fn stop_xray(&mut self) {
        if let Some(mut p) = self.xray.take() {
            p.kill();
        }
    }

    pub fn stop_singbox(&mut self) {
        if let Some(mut p) = self.singbox.take() {
            p.kill();
        }
        self.singbox_config = None;
        self.tun_server = None;
    }

    pub fn tun_address(&mut self, server: &VlessConfig) -> Option<String> {
        let alive = self.singbox.as_mut().is_some_and(|p| p.is_running());
        match &self.tun_server {
            Some((s, ip)) if alive && s == server => Some(ip.clone()),
            _ => None,
        }
    }

    pub fn any_dead(&mut self) -> Option<&'static str> {
        if let Some(p) = self.singbox.as_mut() {
            if !p.is_running() {
                return Some(p.source);
            }
        }
        if let Some(p) = self.xray.as_mut() {
            if !p.is_running() {
                return Some(p.source);
            }
        }
        None
    }
}

pub struct AppState {
    pub events: Arc<dyn EventSink>,
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    pub store: RwLock<ServerStore>,
    pub routing: RwLock<RoutingState>,
    pub conn: RwLock<ConnectionInfo>,
    pub speed_history: RwLock<Vec<SpeedResult>>,
    pub update: RwLock<UpdateStatus>,
    pub last_update_check: AtomicI64,
    pub cores: tokio::sync::Mutex<Cores>,
    pub logs: Arc<LogBuffer>,
    pub job: Option<KillOnCloseJob>,
    pub direct: reqwest::Client,
    pub sysproxy_active: AtomicBool,
    pub session: AtomicU64,
    pub reconnect_seq: AtomicU64,
    pub ping_running: AtomicBool,
    pub speedtest_running: AtomicBool,
    pub speedtest_cancel: AtomicBool,
    pub elevated: bool,
}

pub type Shared = Arc<AppState>;

pub fn direct_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

pub fn socks_client(port: u16, timeout: Duration) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}")).map_err(|e| e.to_string())?;
    reqwest::Client::builder()
        .proxy(proxy)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())
}

impl AppState {
    pub fn new(events: Arc<dyn EventSink>, paths: Paths) -> Self {
        let settings: Settings = load_json(&paths.file(SETTINGS_FILE));
        let store: ServerStore = load_json(&paths.file(SERVERS_FILE));
        let routing: RoutingState = load_json(&paths.file(ROUTING_FILE));
        let speed_history: Vec<SpeedResult> = load_json(&paths.file(SPEED_FILE));
        let conn = ConnectionInfo { mode: settings.routing_mode, tun: settings.tun, ..Default::default() };
        Self {
            events,
            settings: RwLock::new(settings),
            store: RwLock::new(store),
            routing: RwLock::new(routing),
            conn: RwLock::new(conn),
            speed_history: RwLock::new(speed_history),
            update: RwLock::new(UpdateStatus::default()),
            last_update_check: AtomicI64::new(now_secs()),
            cores: tokio::sync::Mutex::new(Cores::default()),
            logs: Arc::new(LogBuffer::new()),
            job: KillOnCloseJob::new(),
            direct: direct_client(Duration::from_secs(30)),
            sysproxy_active: AtomicBool::new(false),
            session: AtomicU64::new(0),
            reconnect_seq: AtomicU64::new(0),
            ping_running: AtomicBool::new(false),
            speedtest_running: AtomicBool::new(false),
            speedtest_cancel: AtomicBool::new(false),
            elevated: crate::win::is_elevated(),
            paths,
        }
    }

    pub fn emit(&self, event: Event) {
        self.events.emit(event);
    }

    pub fn save_settings(&self) {
        let s = self.settings.read().clone();
        if let Err(e) = save_json(&self.paths.file(SETTINGS_FILE), &s) {
            self.logs.error(format!("не удалось сохранить настройки: {e}"));
        }
        self.emit(Event::Settings(s));
    }

    pub fn save_store(&self) {
        let s = self.store.read().clone();
        if let Err(e) = save_json(&self.paths.file(SERVERS_FILE), &s) {
            self.logs.error(format!("не удалось сохранить серверы: {e}"));
        }
        self.emit(Event::Store(s));
    }

    pub fn save_routing(&self) {
        let r = self.routing.read().clone();
        if let Err(e) = save_json(&self.paths.file(ROUTING_FILE), &r) {
            self.logs.error(format!("не удалось сохранить роутинг: {e}"));
        }
        self.emit(Event::Routing(r));
    }

    pub fn set_update(&self, status: UpdateStatus) {
        *self.update.write() = status.clone();
        self.emit(Event::Update(status));
    }

    pub fn update_conn(&self, f: impl FnOnce(&mut ConnectionInfo)) {
        let snapshot = {
            let mut c = self.conn.write();
            f(&mut c);
            c.clone()
        };
        self.emit(Event::Conn(snapshot));
    }

    pub fn status(&self) -> ConnStatus {
        self.conn.read().status
    }

    pub fn is_connected(&self) -> bool {
        self.status() == ConnStatus::Connected
    }

    pub fn find_server(&self, id: &str) -> Option<Server> {
        self.store.read().servers.iter().find(|s| s.id == id).cloned()
    }

    pub fn current_server(&self) -> Option<Server> {
        let selected = self.settings.read().selected_server.clone();
        if let Some(s) = selected.as_deref().and_then(|id| self.find_server(id)) {
            return Some(s);
        }
        let first = self.store.read().servers.first().cloned()?;
        self.settings.write().selected_server = Some(first.id.clone());
        self.save_settings();
        Some(first)
    }

    pub fn tunnel_client(&self, timeout: Duration) -> Option<reqwest::Client> {
        if !self.is_connected() {
            return None;
        }
        socks_client(self.settings.read().socks_port, timeout).ok()
    }

    pub fn next_session(&self) -> u64 {
        self.session.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn session_is(&self, id: u64) -> bool {
        self.session.load(Ordering::SeqCst) == id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleeper() -> CoreProcess {
        let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Windows".into());
        let logs = Arc::new(LogBuffer::new());
        let ping = system.join(r"System32\ping.exe");
        CoreProcess::spawn("sing-box", &ping, &["-n", "30", "127.0.0.1"], &[], &std::env::temp_dir(), &logs, None).unwrap()
    }

    #[test]
    fn live_tun_keeps_its_server_address() {
        let server = VlessConfig { address: "vpn.example.com".into(), port: 443, ..Default::default() };
        let mut cores =
            Cores { singbox: Some(sleeper()), tun_server: Some((server.clone(), "203.0.113.7".into())), ..Default::default() };
        assert_eq!(cores.tun_address(&server).as_deref(), Some("203.0.113.7"));
        let other = VlessConfig { port: 8443, ..server.clone() };
        assert_eq!(cores.tun_address(&other), None, "another server needs a fresh TUN");
        cores.singbox.as_mut().unwrap().kill();
        assert_eq!(cores.tun_address(&server), None, "a stopped TUN is not reused");
        cores.stop();
        assert!(cores.tun_server.is_none() && cores.singbox_config.is_none());
    }
}
