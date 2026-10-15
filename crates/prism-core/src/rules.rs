use crate::routing::geo_list_has;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Proxy,
    Direct,
}

pub struct GeoFiles {
    geosite: Vec<u8>,
    geoip: Vec<u8>,
}

impl GeoFiles {
    pub fn load(geo_dir: &Path) -> Self {
        let read = |name: &str| std::fs::read(geo_dir.join(name)).unwrap_or_default();
        Self { geosite: read("geosite.dat"), geoip: read("geoip.dat") }
    }

    fn has(&self, kind: &str, code: &str) -> bool {
        match kind {
            "geosite" => geo_list_has(&self.geosite, code),
            "geoip" => geo_list_has(&self.geoip, code),
            _ => false,
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub entries: Vec<String>,
    pub rejected: Vec<String>,
}

pub fn parse(text: &str, geo: &GeoFiles) -> Parsed {
    let mut out = Parsed::default();
    for token in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|t| !t.is_empty()) {
        match normalize(token, geo) {
            Some(entry) if !out.entries.contains(&entry) => out.entries.push(entry),
            Some(_) => {}
            None => out.rejected.push(token.to_string()),
        }
    }
    out
}

fn normalize(token: &str, geo: &GeoFiles) -> Option<String> {
    let lower = token.trim().to_lowercase();
    if let Some((kind, code)) = lower.split_once(':').filter(|(k, _)| *k == "geosite" || *k == "geoip") {
        return geo.has(kind, code).then(|| format!("{kind}:{code}"));
    }
    if let Some(net) = parse_subnet(&lower) {
        return Some(net);
    }
    if lower.split_once('/').is_some_and(|(addr, _)| addr.parse::<IpAddr>().is_ok()) {
        return None;
    }
    if let Ok(ip) = lower.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    let host = lower.trim_start_matches("*.");
    let url = if host.contains("://") { url::Url::parse(host) } else { url::Url::parse(&format!("http://{host}")) };
    match url.ok()?.host()? {
        url::Host::Domain(d) => valid_domain(d.trim_end_matches('.')).then(|| d.trim_end_matches('.').to_string()),
        url::Host::Ipv4(ip) => lower.contains(&ip.to_string()).then(|| ip.to_string()),
        url::Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

fn valid_domain(d: &str) -> bool {
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    let numeric_zone = d.rsplit('.').next().is_some_and(|z| z.bytes().all(|b| b.is_ascii_digit()));
    d.len() <= 253 && d.contains('.') && !numeric_zone && d.split('.').all(label_ok)
}

fn parse_subnet(s: &str) -> Option<String> {
    let (addr, prefix) = s.split_once('/')?;
    let prefix: u8 = prefix.parse().ok()?;
    let network = match addr.parse::<IpAddr>().ok()? {
        IpAddr::V4(ip) if prefix <= 32 => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::from((u32::from(ip) & mask).to_be_bytes())
        }
        IpAddr::V6(ip) if prefix <= 128 => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::from((u128::from(ip) & mask).to_be_bytes())
        }
        _ => return None,
    };
    Some(format!("{network}/{prefix}"))
}

pub fn parse_apps(text: &str) -> Parsed {
    let mut out = Parsed::default();
    for token in text.split([',', ';', '\n', '\r']).map(|t| t.trim().trim_matches('"')).filter(|t| !t.is_empty()) {
        match normalize_app(token) {
            Some(app) if !out.entries.iter().any(|e| e.to_lowercase() == app.to_lowercase()) => out.entries.push(app),
            Some(_) => {}
            None => out.rejected.push(token.to_string()),
        }
    }
    out
}

fn normalize_app(token: &str) -> Option<String> {
    let name = token.rsplit(['\\', '/']).next()?.trim();
    let name = if name.contains('.') { name.to_string() } else { format!("{name}.exe") };
    let valid = name.len() > 4
        && name.to_lowercase().ends_with(".exe")
        && !name.chars().any(|c| c.is_control() || "<>:\"|?*".contains(c));
    valid.then_some(name)
}

pub fn app_regex(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for c in name.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!(r"(?i)(^|\\){escaped}$")
}

pub fn available(entries: &[String], geo: &GeoFiles) -> (Vec<String>, Vec<String>) {
    entries.iter().cloned().partition(|e| match e.split_once(':') {
        Some((kind @ ("geosite" | "geoip"), code)) => geo.has(kind, code),
        _ => true,
    })
}

enum Matcher {
    Domain { value: String, depth: usize },
    Ip { value: String, prefix: u8 },
    Site(String),
    IpSet(String),
}

fn matcher(entry: &str) -> Matcher {
    if entry.starts_with("geosite:") {
        return Matcher::Site(entry.to_string());
    }
    if entry.starts_with("geoip:") {
        return Matcher::IpSet(entry.to_string());
    }
    if let Some((addr, prefix)) = entry.split_once('/') {
        if addr.parse::<IpAddr>().is_ok() {
            return Matcher::Ip { value: entry.to_string(), prefix: prefix.parse().unwrap_or(0) };
        }
    }
    match entry.parse::<IpAddr>() {
        Ok(IpAddr::V4(_)) => Matcher::Ip { value: entry.to_string(), prefix: 32 },
        Ok(IpAddr::V6(_)) => Matcher::Ip { value: entry.to_string(), prefix: 128 },
        Err(_) => Matcher::Domain { value: format!("domain:{entry}"), depth: entry.split('.').count() },
    }
}

pub fn xray_rules(proxy: &[String], direct: &[String], proxy_tag: &str, direct_tag: &str) -> Vec<Value> {
    let lists = [(proxy, proxy_tag), (direct, direct_tag)];
    let all = || lists.iter().flat_map(|(entries, tag)| entries.iter().map(move |e| (matcher(e), *tag)));
    let depths: BTreeSet<usize> =
        all().filter_map(|(m, _)| if let Matcher::Domain { depth, .. } = m { Some(depth) } else { None }).collect();
    let prefixes: BTreeSet<u8> =
        all().filter_map(|(m, _)| if let Matcher::Ip { prefix, .. } = m { Some(prefix) } else { None }).collect();

    let mut groups: Vec<(&str, &str, Vec<String>)> = Vec::new();
    for depth in depths.into_iter().rev() {
        for (_, tag) in lists {
            let values = all()
                .filter(|(_, t)| *t == tag)
                .filter_map(|(m, _)| match m {
                    Matcher::Domain { value, depth: d } if d == depth => Some(value),
                    _ => None,
                })
                .collect();
            groups.push(("domain", tag, values));
        }
    }
    for (_, tag) in lists {
        let values = all().filter(|(_, t)| *t == tag).filter_map(|(m, _)| if let Matcher::Site(v) = m { Some(v) } else { None });
        groups.push(("domain", tag, values.collect()));
    }
    for prefix in prefixes.into_iter().rev() {
        for (_, tag) in lists {
            let values = all()
                .filter(|(_, t)| *t == tag)
                .filter_map(|(m, _)| match m {
                    Matcher::Ip { value, prefix: p } if p == prefix => Some(value),
                    _ => None,
                })
                .collect();
            groups.push(("ip", tag, values));
        }
    }
    for (_, tag) in lists {
        let values = all().filter(|(_, t)| *t == tag).filter_map(|(m, _)| if let Matcher::IpSet(v) = m { Some(v) } else { None });
        groups.push(("ip", tag, values.collect()));
    }

    let mut merged: Vec<(&str, &str, Vec<String>)> = Vec::new();
    for (kind, tag, values) in groups.into_iter().filter(|(_, _, v)| !v.is_empty()) {
        match merged.last_mut() {
            Some((k, t, vals)) if *k == kind && *t == tag => vals.extend(values),
            _ => merged.push((kind, tag, values)),
        }
    }
    merged.into_iter().map(|(kind, tag, values)| json!({ kind: values, "outboundTag": tag })).collect()
}

pub fn dns_domains(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match matcher(e) {
            Matcher::Domain { value, .. } | Matcher::Site(value) => Some(value),
            _ => None,
        })
        .collect()
}

pub fn has_ip_rules(entries: &[String]) -> bool {
    entries.iter().any(|e| matches!(matcher(e), Matcher::Ip { .. } | Matcher::IpSet(_)))
}

pub fn display(entry: &str) -> String {
    if entry.contains("xn--") {
        let (unicode, result) = idna::domain_to_unicode(entry);
        if result.is_ok() {
            return unicode;
        }
    }
    entry.to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn geo_file(names: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for name in names {
            let inner = [&[0x0a, name.len() as u8][..], name.as_bytes()].concat();
            out.extend([0x0a, inner.len() as u8]);
            out.extend(inner);
        }
        out
    }

    pub fn geo() -> GeoFiles {
        GeoFiles { geosite: geo_file(&["STEAM", "category-ru"]), geoip: geo_file(&["direct", "private"]) }
    }

    fn entries(text: &str) -> Vec<String> {
        let p = parse(text, &geo());
        assert!(p.rejected.is_empty(), "{text:?} rejected {:?}", p.rejected);
        p.entries
    }

    #[test]
    fn parses_domains() {
        assert_eq!(
            entries("Snowymoon.NET, https://api.example.org:8443/x?y=1;  *.cdn.example.com example.net. snowymoon.net"),
            ["snowymoon.net", "api.example.org", "cdn.example.com", "example.net"]
        );
        assert_eq!(entries("домен.рф https://сайт.рф/путь"), ["xn--d1acufc.xn--p1ai", "xn--80aswg.xn--p1ai"]);
        assert_eq!(entries("my_host.example.com"), ["my_host.example.com"]);
    }

    #[test]
    fn parses_addresses_and_subnets() {
        assert_eq!(
            entries("1.2.3.4 10.1.2.3/8 192.168.0.0/16 2001:DB8::1 [2001:db8::2] 2001:db8:1:2::/32 0.0.0.0/0"),
            ["1.2.3.4", "10.0.0.0/8", "192.168.0.0/16", "2001:db8::1", "2001:db8::2", "2001:db8::/32", "0.0.0.0/0"]
        );
        assert_eq!(entries("http://203.0.113.9:8080/path"), ["203.0.113.9"]);
    }

    #[test]
    fn parses_known_categories() {
        assert_eq!(entries("geosite:Steam GEOIP:direct"), ["geosite:steam", "geoip:direct"]);
        let p = parse("geosite:nothing geoip:steam", &geo());
        assert!(p.entries.is_empty());
        assert_eq!(p.rejected, ["geosite:nothing", "geoip:steam"]);
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["com", "1.2.3", "1.2.3.4/33", "::1/129", "-a.com", "a..com", "a*b.com", "localhost", "foo.123", "full:x.com"] {
            let p = parse(bad, &geo());
            assert!(p.entries.is_empty(), "{bad:?} must be rejected, got {:?}", p.entries);
            assert_eq!(p.rejected, [bad]);
        }
    }

    #[test]
    fn missing_categories_are_dropped() {
        let list = vec!["example.com".to_string(), "geosite:steam".into(), "geosite:gone".into(), "geoip:direct".into()];
        let (kept, dropped) = available(&list, &geo());
        assert_eq!(kept, ["example.com", "geosite:steam", "geoip:direct"]);
        assert_eq!(dropped, ["geosite:gone"]);
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn more_specific_entries_come_first() {
        let proxy = strings(&["google.com", "a.b.example.org", "geosite:steam", "10.0.0.0/8", "1.1.1.1"]);
        let direct = strings(&["translate.google.com", "example.org", "geoip:direct", "10.1.0.0/16"]);
        let rules = xray_rules(&proxy, &direct, "proxy", "direct");
        assert_eq!(
            rules,
            vec![
                json!({ "domain": ["domain:a.b.example.org"], "outboundTag": "proxy" }),
                json!({ "domain": ["domain:translate.google.com"], "outboundTag": "direct" }),
                json!({ "domain": ["domain:google.com"], "outboundTag": "proxy" }),
                json!({ "domain": ["domain:example.org"], "outboundTag": "direct" }),
                json!({ "domain": ["geosite:steam"], "outboundTag": "proxy" }),
                json!({ "ip": ["1.1.1.1"], "outboundTag": "proxy" }),
                json!({ "ip": ["10.1.0.0/16"], "outboundTag": "direct" }),
                json!({ "ip": ["10.0.0.0/8"], "outboundTag": "proxy" }),
                json!({ "ip": ["geoip:direct"], "outboundTag": "direct" }),
            ]
        );
    }

    #[test]
    fn neighbouring_groups_merge() {
        let direct = strings(&["a.example.com", "example.net", "1.2.3.4", "10.0.0.0/8"]);
        let rules = xray_rules(&[], &direct, "proxy", "direct");
        assert_eq!(
            rules,
            vec![
                json!({ "domain": ["domain:a.example.com", "domain:example.net"], "outboundTag": "direct" }),
                json!({ "ip": ["1.2.3.4", "10.0.0.0/8"], "outboundTag": "direct" }),
            ]
        );
        assert!(xray_rules(&[], &[], "proxy", "direct").is_empty());
    }

    #[test]
    fn dns_and_ip_helpers() {
        let list = strings(&["example.com", "geosite:steam", "1.2.3.4", "geoip:direct"]);
        assert_eq!(dns_domains(&list), ["domain:example.com", "geosite:steam"]);
        assert!(has_ip_rules(&list));
        assert!(!has_ip_rules(&strings(&["example.com", "geosite:steam"])));
    }

    #[test]
    fn parses_program_names() {
        let p = parse_apps("Doomsday.exe, C:\\Games\\Last Day\\LastDay.exe; \"steam\"\ngame.EXE\ndoomsday.exe");
        assert_eq!(p.entries, ["Doomsday.exe", "LastDay.exe", "steam.exe", "game.EXE"]);
        assert!(p.rejected.is_empty());
        let p = parse_apps("readme.txt, <bad>.exe, .exe");
        assert!(p.entries.is_empty());
        assert_eq!(p.rejected, ["readme.txt", "<bad>.exe", ".exe"]);
    }

    #[test]
    fn program_regex_ignores_folder_and_case() {
        assert_eq!(app_regex("Doomsday.exe"), r"(?i)(^|\\)Doomsday\.exe$");
        assert_eq!(app_regex("a+b (1).exe"), r"(?i)(^|\\)a\+b \(1\)\.exe$");
    }

    #[test]
    fn shows_international_domains_in_their_letters() {
        assert_eq!(display("xn--d1acufc.xn--p1ai"), "домен.рф");
        assert_eq!(display("example.com"), "example.com");
        assert_eq!(display("10.0.0.0/8"), "10.0.0.0/8");
    }
}
