use crate::subscription::decode_base64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DnsServer {
    pub doh: String,
    pub ip: String,
}

impl DnsServer {
    pub fn xray_address(&self, local: bool) -> String {
        match (self.doh.is_empty(), local) {
            (false, true) => self.doh.replacen("https://", "https+local://", 1),
            (false, false) => self.doh.clone(),
            (true, _) => self.ip.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RoutingProfile {
    pub name: String,
    pub global_proxy: bool,
    pub remote_dns: DnsServer,
    pub domestic_dns: DnsServer,
    pub geoip_url: String,
    pub geosite_url: String,
    pub last_updated: i64,
    pub hosts: BTreeMap<String, String>,
    pub route_order: String,
    pub direct_sites: Vec<String>,
    pub direct_ip: Vec<String>,
    pub proxy_sites: Vec<String>,
    pub proxy_ip: Vec<String>,
    pub block_sites: Vec<String>,
    pub block_ip: Vec<String>,
    pub domain_strategy: String,
}

fn str_of(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn bool_of(v: &Value, key: &str, default: bool) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true") || s == "1",
        _ => default,
    }
}

fn list_of(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn dns_of(v: &Value, prefix: &str, fallback_key: &str) -> DnsServer {
    let kind = str_of(v, &format!("{prefix}DNSType"));
    let ip = match str_of(v, &format!("{prefix}DNSIP")) {
        s if s.is_empty() => str_of(v, fallback_key),
        s => s,
    };
    let domain = str_of(v, &format!("{prefix}DNSDomain"));
    let doh = if kind.eq_ignore_ascii_case("doh") && domain.starts_with("https://") { domain } else { String::new() };
    DnsServer { doh, ip }
}

pub fn parse_deeplink(raw: &str) -> Result<RoutingProfile, String> {
    let raw = raw.trim();
    let payload = raw
        .strip_prefix("happ://routing/onadd/")
        .or_else(|| raw.strip_prefix("happ://routing/add/"))
        .unwrap_or(raw);
    let json_text = if payload.starts_with('{') {
        payload.to_string()
    } else {
        let bytes = decode_base64(payload).ok_or("профиль роутинга: не base64")?;
        String::from_utf8(bytes).map_err(|_| "профиль роутинга: не UTF-8")?
    };
    let v: Value = serde_json::from_str(&json_text).map_err(|e| format!("профиль роутинга: {e}"))?;

    let hosts = v
        .get("DnsHosts")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| val.as_str().map(|ip| (k.clone(), ip.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let profile = RoutingProfile {
        name: str_of(&v, "Name"),
        global_proxy: bool_of(&v, "GlobalProxy", true),
        remote_dns: dns_of(&v, "Remote", "RemoteDns"),
        domestic_dns: dns_of(&v, "Domestic", "DomesticDns"),
        geoip_url: str_of(&v, "Geoipurl"),
        geosite_url: str_of(&v, "Geositeurl"),
        last_updated: str_of(&v, "LastUpdated").parse().unwrap_or(0),
        hosts,
        route_order: match str_of(&v, "RouteOrder") {
            s if s.is_empty() => "block-proxy-direct".into(),
            s => s,
        },
        direct_sites: list_of(&v, "DirectSites"),
        direct_ip: list_of(&v, "DirectIp"),
        proxy_sites: list_of(&v, "ProxySites"),
        proxy_ip: list_of(&v, "ProxyIp"),
        block_sites: list_of(&v, "BlockSites"),
        block_ip: list_of(&v, "BlockIp"),
        domain_strategy: match str_of(&v, "DomainStrategy") {
            s if s.is_empty() => "IPIfNonMatch".into(),
            s => s,
        },
    };
    if profile.remote_dns.ip.is_empty() && profile.remote_dns.doh.is_empty() {
        return Err("профиль роутинга: не указан DNS".into());
    }
    Ok(profile)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoutingState {
    pub source_url: String,
    pub fetched_at: Option<i64>,
    pub profile: RoutingProfile,
    pub geoip_url: String,
    pub geosite_url: String,
    pub last_error: Option<String>,
}

pub async fn download_to(client: &reqwest::Client, url: &str, dest: &Path) -> Result<(), String> {
    let resp = client.get(url).send().await.map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    if bytes.len() < 1024 {
        return Err(format!("{url}: подозрительно маленький файл"));
    }
    let tmp = dest.with_extension("download");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

fn visit_bytes_fields(msg: &[u8], mut visit: impl FnMut(u64, &[u8]) -> bool) -> Option<()> {
    let mut pos = 0;
    while pos < msg.len() {
        let key = read_varint(msg, &mut pos)?;
        match key & 7 {
            0 => {
                read_varint(msg, &mut pos)?;
            }
            1 => pos += 8,
            2 => {
                let len = usize::try_from(read_varint(msg, &mut pos)?).ok()?;
                let body = msg.get(pos..pos.checked_add(len)?)?;
                pos += len;
                if !visit(key >> 3, body) {
                    return Some(());
                }
            }
            5 => pos += 4,
            _ => return None,
        }
    }
    Some(())
}

pub fn geoip_has(path: &Path, code: &str) -> bool {
    std::fs::read(path).is_ok_and(|data| geo_list_has(&data, code))
}

pub(crate) fn geo_list_has(data: &[u8], code: &str) -> bool {
    let mut found = false;
    visit_bytes_fields(data, |field, entry| {
        if field == 1 {
            visit_bytes_fields(entry, |f, value| {
                if f == 1 {
                    found = value.eq_ignore_ascii_case(code.as_bytes());
                }
                f != 1
            });
        }
        !found
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_geoip_categories() {
        let data = [
            0x0a, 0x08, 0x0a, 0x02, b'R', b'U', 0x12, 0x02, 0x10, 0x18,
            0x0a, 0x0b, 0x0a, 0x09, b'W', b'H', b'I', b'T', b'E', b'L', b'I', b'S', b'T',
        ];
        assert!(geo_list_has(&data, "whitelist"));
        assert!(geo_list_has(&data, "ru"));
        assert!(!geo_list_has(&data, "direct"));
        assert!(!geo_list_has(&data[..5], "ru"), "truncated data finds nothing");

        let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/geo/geoip.dat");
        if bundled.exists() {
            assert!(geoip_has(&bundled, "whitelist"));
            assert!(geoip_has(&bundled, "direct"));
            assert!(!geoip_has(&bundled, "nonexistent"));
        }
    }

    const DEEPLINK: &str = include_str!("../tests/fixtures/DEFAULT.DEEPLINK");

    #[test]
    fn parses_roscomvpn_profile() {
        let p = parse_deeplink(DEEPLINK).unwrap();
        assert_eq!(p.name, "RoscomVPN");
        assert!(p.global_proxy);
        assert_eq!(p.remote_dns.doh, "https://8.8.8.8/dns-query");
        assert_eq!(p.domestic_dns.doh, "https://77.88.8.8/dns-query");
        assert_eq!(p.domestic_dns.xray_address(true), "https+local://77.88.8.8/dns-query");
        assert_eq!(p.route_order, "block-proxy-direct");
        assert_eq!(p.domain_strategy, "IPIfNonMatch");
        assert!(p.direct_sites.contains(&"geosite:category-ru".to_string()));
        assert!(p.direct_ip.contains(&"geoip:direct".to_string()));
        assert!(p.proxy_sites.contains(&"geosite:youtube".to_string()));
        assert!(p.block_sites.contains(&"geosite:category-ads".to_string()));
        assert!(p.geoip_url.ends_with("geoip.dat"));
        assert!(p.geosite_url.ends_with("geosite.dat"));
        assert!(p.last_updated > 1_700_000_000);
        assert_eq!(p.hosts.get("lknpd.nalog.ru").map(String::as_str), Some("213.24.64.181"));
    }

    #[test]
    fn plain_ip_dns_fallback() {
        let p = parse_deeplink(r#"{"RemoteDns":"1.1.1.1","DomesticDns":"77.88.8.8","RemoteDNSType":"DoU"}"#).unwrap();
        assert_eq!(p.remote_dns.xray_address(false), "1.1.1.1");
        assert_eq!(p.domestic_dns.xray_address(true), "77.88.8.8");
        assert_eq!(p.route_order, "block-proxy-direct");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_deeplink("happ://routing/onadd/!!!").is_err());
        assert!(parse_deeplink("{}").is_err());
    }
}
