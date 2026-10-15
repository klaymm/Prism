use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct VlessConfig {
    pub address: String,
    pub port: u16,
    pub uuid: String,
    pub encryption: String,
    pub flow: String,
    pub security: String,
    pub sni: String,
    pub fingerprint: String,
    pub alpn: Vec<String>,
    pub allow_insecure: bool,
    pub public_key: String,
    pub short_id: String,
    pub spider_x: String,
    pub mldsa65_verify: String,
    pub network: String,
    pub header_type: String,
    pub host: String,
    pub path: String,
    pub service_name: String,
    pub authority: String,
    pub mode: String,
    pub extra: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LatencyKind {
    Real,
    Tcp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Latency {
    pub ms: i64,
    pub kind: LatencyKind,
    pub at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub subscription_id: Option<String>,
    pub name: String,
    pub link: String,
    pub config: VlessConfig,
    #[serde(default)]
    pub latency: Option<Latency>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub url: String,
    pub updated_at: Option<i64>,
    pub update_interval_hours: Option<u32>,
    pub upload: Option<u64>,
    pub download: Option<u64>,
    pub total: Option<u64>,
    pub expire: Option<i64>,
    pub support_url: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerStore {
    pub subscriptions: Vec<Subscription>,
    pub servers: Vec<Server>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    #[default]
    Smart,
    Global,
}

pub const DEFAULT_ROUTING_URL: &str =
    "https://raw.githubusercontent.com/hydraponique/roscomvpn-routing/refs/heads/main/HAPP/DEFAULT.DEEPLINK";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: String,
    pub routing_mode: RoutingMode,
    pub tun: bool,
    pub selected_server: Option<String>,
    pub socks_port: u16,
    pub http_port: u16,
    pub allow_lan: bool,
    pub autostart: bool,
    pub connect_on_launch: bool,
    pub close_to_tray: bool,
    pub auto_update: bool,
    pub user_agent: String,
    pub routing_url: String,
    pub ru_direct: bool,
    pub proxy_rules: Vec<String>,
    #[serde(alias = "directDomains")]
    pub direct_rules: Vec<String>,
    pub direct_apps: Vec<String>,
    pub tun_stack: String,
    pub tun_mtu: u32,
    pub log_level: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            routing_mode: RoutingMode::Smart,
            tun: false,
            selected_server: None,
            socks_port: 10818,
            http_port: 10819,
            allow_lan: false,
            autostart: false,
            connect_on_launch: false,
            close_to_tray: true,
            auto_update: true,
            user_agent: "v2rayN/7.13.8".into(),
            routing_url: DEFAULT_ROUTING_URL.into(),
            ru_direct: true,
            proxy_rules: Vec::new(),
            direct_rules: Vec::new(),
            direct_apps: Vec::new(),
            tun_stack: "mixed".into(),
            tun_mtu: 9000,
            log_level: "warning".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConnStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Error,
}

#[derive(Clone, Debug, Default)]
pub struct ConnectionInfo {
    pub status: ConnStatus,
    pub server_id: Option<String>,
    pub mode: RoutingMode,
    pub tun: bool,
    pub since: Option<i64>,
    pub error: Option<String>,
    pub note: Option<String>,
    pub ip: Option<String>,
    pub country: Option<String>,
    pub ip_pending: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedResult {
    pub server_name: String,
    pub download_mbps: f64,
    pub upload_mbps: f64,
    pub ping_ms: f64,
    pub jitter_ms: f64,
    pub at: i64,
}

pub fn display_name(name: &str) -> String {
    let stripped: String = name.chars().filter(|c| !('\u{1F1E6}'..='\u{1F1FF}').contains(c)).collect();
    let trimmed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() { name.to_string() } else { trimmed }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_direct_list_is_kept() {
        let s: Settings = serde_json::from_str(r#"{"directDomains":["snowymoon.net"],"tun":true}"#).unwrap();
        assert_eq!(s.direct_rules, ["snowymoon.net"]);
        assert!(s.proxy_rules.is_empty());
        assert!(s.tun);
        let saved = serde_json::to_value(&s).unwrap();
        assert_eq!(saved["directRules"], serde_json::json!(["snowymoon.net"]));
        assert!(saved.get("directDomains").is_none());
    }

    #[test]
    fn display_name_strips_flags() {
        assert_eq!(super::display_name("🇳🇱 Amsterdam"), "Amsterdam");
        assert_eq!(super::display_name("Frankfurt 🇩🇪  #2"), "Frankfurt #2");
        assert_eq!(super::display_name("🇩🇪"), "🇩🇪");
    }
}
