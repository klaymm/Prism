use crate::models::{RoutingMode, VlessConfig};
use crate::routing::RoutingProfile;
use serde_json::{Map, Value, json};

pub const PROXY: &str = "proxy";
pub const DIRECT: &str = "direct";
pub const BLOCK: &str = "block";
const DNS_OUT: &str = "dns-out";
const DNS_TAG: &str = "dns-internal";

pub const RU_ZONES: [&str; 3] = ["domain:ru", "domain:su", "domain:xn--p1ai"];
pub const RU_CLOUD_IP: &str = "geoip:whitelist";

pub fn has_ru_cloud_ip(geoip_dat: &std::path::Path) -> bool {
    crate::routing::geoip_has(geoip_dat, RU_CLOUD_IP.trim_start_matches("geoip:"))
}

fn or<'a>(v: &'a str, fallback: &'a str) -> &'a str {
    if v.is_empty() { fallback } else { v }
}

fn split_csv(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

fn stream_settings(c: &VlessConfig) -> Value {
    let network = or(&c.network, "tcp");
    let security = or(&c.security, "none");
    let mut ss = Map::new();
    ss.insert("network".into(), json!(network));
    ss.insert("security".into(), json!(security));

    match security {
        "reality" => {
            let mut r = json!({
                "serverName": c.sni,
                "fingerprint": or(&c.fingerprint, "chrome"),
                "publicKey": c.public_key,
                "shortId": c.short_id,
                "spiderX": c.spider_x,
            });
            if !c.mldsa65_verify.is_empty() {
                r["mldsa65Verify"] = json!(c.mldsa65_verify);
            }
            ss.insert("realitySettings".into(), r);
        }
        "tls" => {
            let mut t = json!({
                "serverName": or(&c.sni, or(&c.host, &c.address)),
                "fingerprint": or(&c.fingerprint, "chrome"),
            });
            if !c.alpn.is_empty() {
                t["alpn"] = json!(c.alpn);
            }
            if c.allow_insecure {
                t["allowInsecure"] = json!(true);
            }
            ss.insert("tlsSettings".into(), t);
        }
        _ => {}
    }

    match network {
        "tcp" if c.header_type == "http" => {
            ss.insert(
                "tcpSettings".into(),
                json!({ "header": { "type": "http", "request": {
                    "path": split_csv(or(&c.path, "/")),
                    "headers": { "Host": split_csv(&c.host) }
                }}}),
            );
        }
        "xhttp" => {
            let mut x = json!({ "path": or(&c.path, "/"), "mode": or(&c.mode, "auto") });
            if !c.host.is_empty() {
                x["host"] = json!(c.host);
            }
            if let Some(extra) = &c.extra {
                x["extra"] = extra.clone();
            }
            ss.insert("xhttpSettings".into(), x);
        }
        "grpc" => {
            let mut g = json!({ "serviceName": c.service_name, "multiMode": c.mode == "multi" });
            if !c.authority.is_empty() {
                g["authority"] = json!(c.authority);
            }
            ss.insert("grpcSettings".into(), g);
        }
        "ws" => {
            ss.insert("wsSettings".into(), json!({ "path": or(&c.path, "/"), "host": c.host }));
        }
        "httpupgrade" => {
            ss.insert("httpupgradeSettings".into(), json!({ "path": or(&c.path, "/"), "host": c.host }));
        }
        "kcp" => {
            ss.insert("kcpSettings".into(), json!({ "header": { "type": or(&c.header_type, "none") } }));
        }
        _ => {}
    }
    Value::Object(ss)
}

pub fn outbound(tag: &str, c: &VlessConfig, address_override: Option<&str>) -> Value {
    let mut user = json!({ "id": c.uuid, "encryption": or(&c.encryption, "none"), "level": 0 });
    if !c.flow.is_empty() {
        user["flow"] = json!(c.flow);
    }
    json!({
        "tag": tag,
        "protocol": "vless",
        "settings": { "vnext": [{
            "address": address_override.unwrap_or(&c.address),
            "port": c.port,
            "users": [user],
        }]},
        "streamSettings": stream_settings(c),
    })
}

fn sniffing() -> Value {
    json!({ "enabled": true, "destOverride": ["http", "tls", "quic"], "routeOnly": false })
}

pub struct MainInput<'a> {
    pub server: &'a VlessConfig,
    pub server_address: Option<&'a str>,
    pub mode: RoutingMode,
    pub profile: &'a RoutingProfile,
    pub socks_port: u16,
    pub http_port: u16,
    pub allow_lan: bool,
    pub metrics_port: u16,
    pub log_level: &'a str,
    pub ru_direct: bool,
    pub ru_cloud_ip: bool,
    pub proxy_rules: &'a [String],
    pub direct_rules: &'a [String],
}

fn direct_lists(input: &MainInput) -> (Vec<String>, Vec<String>) {
    let p = input.profile;
    let (mut sites, mut ips) = (p.direct_sites.clone(), p.direct_ip.clone());
    if input.ru_direct {
        for zone in RU_ZONES {
            if !sites.iter().any(|s| s == zone) {
                sites.push(zone.into());
            }
        }
        if input.ru_cloud_ip && !ips.iter().any(|i| i == RU_CLOUD_IP) {
            ips.push(RU_CLOUD_IP.into());
        }
    }
    (sites, ips)
}

fn rule(kind: &str, values: &[String], outbound: &str) -> Option<Value> {
    (!values.is_empty()).then(|| json!({ kind: values, "outboundTag": outbound }))
}

fn is_domain_matcher(s: &str) -> bool {
    !s.starts_with("geoip:") && !s.contains('/') && s.parse::<std::net::IpAddr>().is_err()
}

fn smart_rules(input: &MainInput) -> Vec<Value> {
    let p = input.profile;
    let (direct_sites, direct_ip) = direct_lists(input);
    let mut rules = Vec::new();
    for group in p.route_order.split('-') {
        let (sites, ips, tag) = match group.trim() {
            "block" => (&p.block_sites, &p.block_ip, BLOCK),
            "proxy" => (&p.proxy_sites, &p.proxy_ip, PROXY),
            "direct" => (&direct_sites, &direct_ip, DIRECT),
            _ => continue,
        };
        rules.extend(rule("domain", sites, tag));
        rules.extend(rule("ip", ips, tag));
    }
    if !p.global_proxy {
        rules.push(json!({ "network": "tcp,udp", "outboundTag": DIRECT }));
    }
    rules
}

fn dns(input: &MainInput) -> Value {
    let p = input.profile;
    let remote = p.remote_dns.xray_address(false);
    let mut servers = vec![json!(remote)];
    let proxied = crate::rules::dns_domains(input.proxy_rules);
    if !proxied.is_empty() {
        servers.push(json!({ "address": remote, "domains": proxied, "skipFallback": true }));
    }
    let mut domestic_domains: Vec<String> = Vec::new();
    if input.mode == RoutingMode::Smart {
        domestic_domains.extend(direct_lists(input).0.into_iter().filter(|s| is_domain_matcher(s)));
    }
    for domain in crate::rules::dns_domains(input.direct_rules) {
        if !domestic_domains.contains(&domain) {
            domestic_domains.push(domain);
        }
    }
    if !domestic_domains.is_empty() {
        servers.push(json!({
            "address": p.domestic_dns.xray_address(true),
            "domains": domestic_domains,
            "skipFallback": true,
        }));
    }
    json!({
        "hosts": p.hosts,
        "servers": servers,
        "queryStrategy": "UseIPv4",
        "tag": DNS_TAG,
    })
}

pub fn build_main(input: &MainInput) -> Value {
    let listen = if input.allow_lan { "0.0.0.0" } else { "127.0.0.1" };

    let mut rules = vec![
        json!({ "inboundTag": [DNS_TAG], "outboundTag": PROXY }),
        json!({ "network": "udp", "port": "53", "outboundTag": DNS_OUT }),
    ];
    rules.extend(crate::rules::xray_rules(input.proxy_rules, input.direct_rules, PROXY, DIRECT));
    let domain_strategy = match input.mode {
        RoutingMode::Smart => {
            rules.extend(smart_rules(input));
            input.profile.domain_strategy.as_str()
        }
        RoutingMode::Global => {
            rules.push(json!({ "ip": ["geoip:private"], "outboundTag": DIRECT }));
            let ip_rules = crate::rules::has_ip_rules(input.proxy_rules) || crate::rules::has_ip_rules(input.direct_rules);
            if ip_rules { "IPIfNonMatch" } else { "AsIs" }
        }
    };

    json!({
        "log": { "loglevel": input.log_level, "access": "none", "dnsLog": false },
        "stats": {},
        "metrics": { "tag": "metrics", "listen": format!("127.0.0.1:{}", input.metrics_port) },
        "policy": { "system": { "statsOutboundUplink": true, "statsOutboundDownlink": true } },
        "dns": dns(input),
        "inbounds": [
            {
                "tag": "socks-in", "listen": listen, "port": input.socks_port, "protocol": "socks",
                "settings": { "auth": "noauth", "udp": true }, "sniffing": sniffing(),
            },
            {
                "tag": "http-in", "listen": listen, "port": input.http_port, "protocol": "http",
                "settings": {}, "sniffing": sniffing(),
            },
        ],
        "outbounds": [
            outbound(PROXY, input.server, input.server_address),
            { "tag": DIRECT, "protocol": "freedom", "settings": { "domainStrategy": "UseIPv4" } },
            { "tag": BLOCK, "protocol": "blackhole" },
            { "tag": DNS_OUT, "protocol": "dns" },
        ],
        "routing": { "domainStrategy": domain_strategy, "rules": rules },
    })
}

pub fn build_probe(servers: &[(u16, &VlessConfig)]) -> Value {
    let mut inbounds = Vec::new();
    let mut outbounds = Vec::new();
    let mut rules = Vec::new();
    for (i, (port, cfg)) in servers.iter().enumerate() {
        let (inb, out) = (format!("in-{i}"), format!("out-{i}"));
        inbounds.push(json!({
            "tag": inb, "listen": "127.0.0.1", "port": port, "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }));
        outbounds.push(outbound(&out, cfg, None));
        rules.push(json!({ "inboundTag": [inb], "outboundTag": out }));
    }
    json!({
        "log": { "loglevel": "none" },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "routing": { "rules": rules },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::parse_deeplink;
    use crate::share_link::parse_vless;

    pub fn sample_input_parts() -> (VlessConfig, RoutingProfile) {
        let (_, c) = parse_vless("vless://b831381d-6324-4d53-ad4f-8cda48b30811@203.0.113.7:443?flow=xtls-rprx-vision&security=reality&sni=www.microsoft.com&fp=chrome&pbk=Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw&sid=6ba85179e30d4fc2&type=tcp#NL").unwrap();
        let p = parse_deeplink(include_str!("../tests/fixtures/DEFAULT.DEEPLINK")).unwrap();
        (c, p)
    }

    fn input<'a>(c: &'a VlessConfig, p: &'a RoutingProfile, mode: RoutingMode) -> MainInput<'a> {
        MainInput {
            server: c,
            server_address: None,
            mode,
            profile: p,
            socks_port: 10808,
            http_port: 10809,
            allow_lan: false,
            metrics_port: 10813,
            log_level: "warning",
            ru_direct: false,
            ru_cloud_ip: false,
            proxy_rules: &[],
            direct_rules: &[],
        }
    }

    fn ru_input<'a>(c: &'a VlessConfig, p: &'a RoutingProfile, cloud: bool) -> MainInput<'a> {
        MainInput { ru_direct: true, ru_cloud_ip: cloud, ..input(c, p, RoutingMode::Smart) }
    }

    fn direct_rule<'a>(cfg: &'a Value, kind: &str) -> &'a Vec<Value> {
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        let rule = rules.iter().find(|r| r["outboundTag"] == DIRECT && r.get(kind).is_some()).unwrap();
        rule[kind].as_array().unwrap()
    }

    #[test]
    fn ru_direct_adds_zones_clouds_and_domestic_dns() {
        let (c, p) = sample_input_parts();
        let cfg = build_main(&ru_input(&c, &p, true));
        let sites = direct_rule(&cfg, "domain");
        for zone in RU_ZONES {
            assert!(sites.iter().any(|s| s == zone), "{zone} missing");
        }
        assert!(direct_rule(&cfg, "ip").iter().any(|i| i == RU_CLOUD_IP));
        let domestic = cfg["dns"]["servers"][1]["domains"].as_array().unwrap();
        assert!(domestic.iter().any(|d| d == "domain:ru"));
        let tags: Vec<&str> = cfg["routing"]["rules"].as_array().unwrap().iter().map(|r| r["outboundTag"].as_str().unwrap()).collect();
        assert_eq!(tags, vec![PROXY, DNS_OUT, BLOCK, PROXY, DIRECT, DIRECT], "proxy rules still come first");
    }

    #[test]
    fn ru_direct_skips_cloud_ips_missing_from_geoip() {
        let (c, p) = sample_input_parts();
        let cfg = build_main(&ru_input(&c, &p, false));
        assert!(direct_rule(&cfg, "domain").iter().any(|s| s == "domain:ru"));
        assert!(!direct_rule(&cfg, "ip").iter().any(|i| i == RU_CLOUD_IP));
    }

    #[test]
    fn ru_direct_off_keeps_profile_lists() {
        let (c, p) = sample_input_parts();
        let cfg = build_main(&input(&c, &p, RoutingMode::Smart));
        assert_eq!(direct_rule(&cfg, "domain").len(), p.direct_sites.len());
        assert_eq!(direct_rule(&cfg, "ip").len(), p.direct_ip.len());
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn user_rules_come_before_profile_rules() {
        let (c, p) = sample_input_parts();
        let proxy = strings(&["kinopoisk.ru"]);
        let direct = strings(&["snowymoon.net", "203.0.113.0/24"]);
        let cfg = build_main(&MainInput { proxy_rules: &proxy, direct_rules: &direct, ..input(&c, &p, RoutingMode::Smart) });
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules[2], json!({ "domain": ["domain:kinopoisk.ru"], "outboundTag": PROXY }));
        assert_eq!(rules[3], json!({ "domain": ["domain:snowymoon.net"], "outboundTag": DIRECT }));
        assert_eq!(rules[4], json!({ "ip": ["203.0.113.0/24"], "outboundTag": DIRECT }));
        assert_eq!(rules[5]["outboundTag"], BLOCK, "profile rules follow the user's");
        let servers = cfg["dns"]["servers"].as_array().unwrap();
        assert_eq!(
            servers[1],
            json!({ "address": "https://8.8.8.8/dns-query", "domains": ["domain:kinopoisk.ru"], "skipFallback": true })
        );
        let domestic = servers[2]["domains"].as_array().unwrap();
        assert!(domestic.iter().any(|d| d == "domain:snowymoon.net"));
        assert!(domestic.iter().any(|d| d == "geosite:whitelist"), "profile domains stay in the domestic list");
    }

    #[test]
    fn user_rules_work_in_global_mode() {
        let (c, p) = sample_input_parts();
        let direct = strings(&["snowymoon.net"]);
        let cfg = build_main(&MainInput { direct_rules: &direct, ..input(&c, &p, RoutingMode::Global) });
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[2], json!({ "domain": ["domain:snowymoon.net"], "outboundTag": DIRECT }));
        assert_eq!(rules[3]["ip"][0], "geoip:private");
        assert_eq!(cfg["routing"]["domainStrategy"], "AsIs");
        let dns_servers = cfg["dns"]["servers"].as_array().unwrap();
        assert_eq!(dns_servers.len(), 2);
        assert_eq!(dns_servers[1]["domains"], json!(["domain:snowymoon.net"]));

        let subnet = strings(&["203.0.113.0/24"]);
        let cfg = build_main(&MainInput { direct_rules: &subnet, ..input(&c, &p, RoutingMode::Global) });
        assert_eq!(cfg["routing"]["domainStrategy"], "IPIfNonMatch", "IP entries need names resolved");
    }

    #[test]
    fn smart_mode_orders_block_proxy_direct() {
        let (c, p) = sample_input_parts();
        let cfg = build_main(&input(&c, &p, RoutingMode::Smart));
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        let tags: Vec<&str> = rules.iter().map(|r| r["outboundTag"].as_str().unwrap()).collect();
        assert_eq!(tags, vec![PROXY, DNS_OUT, BLOCK, PROXY, DIRECT, DIRECT]);
        assert_eq!(cfg["routing"]["domainStrategy"], "IPIfNonMatch");
        assert_eq!(cfg["outbounds"][0]["tag"], PROXY);
        let dns_servers = cfg["dns"]["servers"].as_array().unwrap();
        assert_eq!(dns_servers[0], "https://8.8.8.8/dns-query");
        assert_eq!(dns_servers[1]["address"], "https+local://77.88.8.8/dns-query");
        assert!(dns_servers[1]["domains"].as_array().unwrap().iter().any(|d| d == "geosite:whitelist"));
        assert_eq!(cfg["dns"]["hosts"]["lkfl2.nalog.ru"], "213.24.64.175");
    }

    #[test]
    fn global_mode_sends_everything_to_proxy() {
        let (c, p) = sample_input_parts();
        let cfg = build_main(&input(&c, &p, RoutingMode::Global));
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[2]["ip"][0], "geoip:private");
        assert_eq!(cfg["dns"]["servers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn reality_outbound_shape() {
        let (c, _) = sample_input_parts();
        let ob = outbound(PROXY, &c, Some("198.51.100.1"));
        assert_eq!(ob["settings"]["vnext"][0]["address"], "198.51.100.1");
        assert_eq!(ob["settings"]["vnext"][0]["users"][0]["flow"], "xtls-rprx-vision");
        let rs = &ob["streamSettings"]["realitySettings"];
        assert_eq!(rs["serverName"], "www.microsoft.com");
        assert_eq!(rs["publicKey"], "Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw");
    }

    #[test]
    fn probe_pairs_inbounds_with_outbounds() {
        let (c, _) = sample_input_parts();
        let cfg = build_probe(&[(20001, &c), (20002, &c)]);
        assert_eq!(cfg["inbounds"][1]["port"], 20002);
        assert_eq!(cfg["routing"]["rules"][1]["outboundTag"], "out-1");
    }

    #[test]
    fn dump_configs_for_core_validation() {
        let (c, p) = sample_input_parts();
        let dir = crate::test_output_dir("prism-config-check");
        for (name, mode) in [("smart", RoutingMode::Smart), ("global", RoutingMode::Global)] {
            let cfg = build_main(&input(&c, &p, mode));
            std::fs::write(dir.join(format!("xray-{name}.json")), serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        }
        let ru = build_main(&ru_input(&c, &p, true));
        std::fs::write(dir.join("xray-smart-ru.json"), serde_json::to_string_pretty(&ru).unwrap()).unwrap();
        let proxy = strings(&["kinopoisk.ru", "geosite:youtube", "2001:db8::/32", "geoip:private"]);
        let direct = strings(&["snowymoon.net", "xn--d1acufc.xn--p1ai", "geosite:steam", "203.0.113.0/24", "geoip:direct"]);
        for (name, mode) in [("smart", RoutingMode::Smart), ("global", RoutingMode::Global)] {
            let cfg = build_main(&MainInput { proxy_rules: &proxy, direct_rules: &direct, ..input(&c, &p, mode) });
            std::fs::write(dir.join(format!("xray-{name}-rules.json")), serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        }
        let probe = build_probe(&[(20001, &c)]);
        std::fs::write(dir.join("xray-probe.json"), serde_json::to_string_pretty(&probe).unwrap()).unwrap();
    }
}
