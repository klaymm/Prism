use crate::models::VlessConfig;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum LinkError {
    #[error("неподдерживаемая схема ссылки")]
    UnsupportedScheme,
    #[error("некорректная ссылка: {0}")]
    Malformed(&'static str),
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn query_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

fn split_host_port(s: &str) -> Result<(String, u16), LinkError> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']').ok_or(LinkError::Malformed("IPv6-адрес"))?;
        let port = rest[end + 1..].strip_prefix(':').ok_or(LinkError::Malformed("порт"))?;
        (rest[..end].to_string(), port)
    } else {
        let (h, p) = s.rsplit_once(':').ok_or(LinkError::Malformed("порт"))?;
        (h.to_string(), p)
    };
    let port = port.parse::<u16>().map_err(|_| LinkError::Malformed("порт"))?;
    if host.is_empty() || port == 0 {
        return Err(LinkError::Malformed("адрес"));
    }
    Ok((host, port))
}

pub fn is_share_link(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    ["vless://", "vmess://", "trojan://", "ss://", "hysteria2://", "hy2://", "tuic://"]
        .iter()
        .any(|p| lower.starts_with(p))
}

pub fn parse_vless(link: &str) -> Result<(String, VlessConfig), LinkError> {
    let link = link.trim();
    let rest = link
        .strip_prefix("vless://")
        .or_else(|| link.strip_prefix("VLESS://"))
        .ok_or(LinkError::UnsupportedScheme)?;

    let (rest, name) = match rest.split_once('#') {
        Some((r, n)) => (r, decode(n)),
        None => (rest, String::new()),
    };
    let (authority, query) = match rest.split_once('?') {
        Some((a, q)) => (a, q),
        None => (rest, ""),
    };
    let authority = authority.trim_end_matches('/');
    let (uuid, hostport) = authority.rsplit_once('@').ok_or(LinkError::Malformed("нет UUID"))?;
    let uuid = decode(uuid);
    if uuid.is_empty() {
        return Err(LinkError::Malformed("нет UUID"));
    }
    let (address, port) = split_host_port(hostport)?;

    let mut c = VlessConfig {
        address,
        port,
        uuid,
        encryption: "none".into(),
        security: "none".into(),
        network: "tcp".into(),
        ..Default::default()
    };

    for (k, v) in query_params(query) {
        match k.as_str() {
            "encryption" if !v.is_empty() => c.encryption = v,
            "flow" => c.flow = v,
            "security" if !v.is_empty() => c.security = v.to_ascii_lowercase(),
            "sni" | "peer" if !v.is_empty() => c.sni = v,
            "fp" => c.fingerprint = v,
            "alpn" => {
                c.alpn = v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
            }
            "allowInsecure" | "insecure" | "allow_insecure" => c.allow_insecure = v == "1" || v == "true",
            "pbk" => c.public_key = v,
            "sid" => c.short_id = v,
            "spx" => c.spider_x = v,
            "pqv" => c.mldsa65_verify = v,
            "type" if !v.is_empty() => c.network = v.to_ascii_lowercase(),
            "headerType" => c.header_type = v,
            "host" => c.host = v,
            "path" => c.path = v,
            "serviceName" => c.service_name = v,
            "authority" => c.authority = v,
            "mode" => c.mode = v,
            "extra" if !v.is_empty() => c.extra = serde_json::from_str(&v).ok(),
            _ => {}
        }
    }
    if c.network == "raw" {
        c.network = "tcp".into();
    }
    if c.network == "splithttp" {
        c.network = "xhttp".into();
    }

    let name = if name.trim().is_empty() {
        format!("{}:{}", c.address, c.port)
    } else {
        name.trim().to_string()
    };
    Ok((name, c))
}

const COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'/')
    .add(b':')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

pub fn to_vless_link(name: &str, c: &VlessConfig) -> String {
    let host = if c.address.contains(':') { format!("[{}]", c.address) } else { c.address.clone() };
    let mut params: Vec<(&str, String)> = vec![
        ("encryption", if c.encryption.is_empty() { "none".into() } else { c.encryption.clone() }),
        ("security", c.security.clone()),
        ("type", c.network.clone()),
    ];
    let mut push = |k: &'static str, v: &str| {
        if !v.is_empty() {
            params.push((k, v.to_string()));
        }
    };
    push("flow", &c.flow);
    push("sni", &c.sni);
    push("fp", &c.fingerprint);
    push("alpn", &c.alpn.join(","));
    push("pbk", &c.public_key);
    push("sid", &c.short_id);
    push("spx", &c.spider_x);
    push("pqv", &c.mldsa65_verify);
    push("headerType", &c.header_type);
    push("host", &c.host);
    push("path", &c.path);
    push("serviceName", &c.service_name);
    push("authority", &c.authority);
    push("mode", &c.mode);
    if c.allow_insecure {
        params.push(("allowInsecure", "1".into()));
    }
    if let Some(extra) = &c.extra {
        params.push(("extra", extra.to_string()));
    }
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("vless://{}@{}:{}?{}#{}", enc(&c.uuid), host, c.port, query, enc(name))
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn from_xray_outbound(ob: &Value) -> Option<VlessConfig> {
    if ob.get("protocol").and_then(Value::as_str) != Some("vless") {
        return None;
    }
    let settings = ob.get("settings")?;
    let (server, user) = match settings.get("vnext").and_then(|v| v.get(0)) {
        Some(vnext) => (vnext, vnext.get("users").and_then(|u| u.get(0))?),
        None => (settings, settings),
    };
    let port = server.get("port").and_then(Value::as_u64)? as u16;
    let mut c = VlessConfig {
        address: s(server, "address"),
        port,
        uuid: s(user, "id"),
        encryption: user.get("encryption").and_then(Value::as_str).unwrap_or("none").to_string(),
        flow: s(user, "flow"),
        security: "none".into(),
        network: "tcp".into(),
        ..Default::default()
    };
    if c.address.is_empty() || c.uuid.is_empty() {
        return None;
    }

    let empty = Value::Null;
    let ss = ob.get("streamSettings").unwrap_or(&empty);
    if let Some(n) = ss.get("network").and_then(Value::as_str) {
        c.network = match n {
            "raw" => "tcp".into(),
            "splithttp" => "xhttp".into(),
            other => other.to_string(),
        };
    }
    if let Some(sec) = ss.get("security").and_then(Value::as_str) {
        c.security = sec.to_string();
    }
    if let Some(r) = ss.get("realitySettings") {
        c.sni = s(r, "serverName");
        c.fingerprint = s(r, "fingerprint");
        c.public_key = s(r, "publicKey");
        c.short_id = s(r, "shortId");
        c.spider_x = s(r, "spiderX");
        c.mldsa65_verify = s(r, "mldsa65Verify");
    }
    if let Some(t) = ss.get("tlsSettings") {
        c.sni = s(t, "serverName");
        c.fingerprint = s(t, "fingerprint");
        c.allow_insecure = t.get("allowInsecure").and_then(Value::as_bool).unwrap_or(false);
        c.alpn = t
            .get("alpn")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
    }
    let transport = match c.network.as_str() {
        "xhttp" => ss.get("xhttpSettings").or_else(|| ss.get("splithttpSettings")),
        "grpc" => ss.get("grpcSettings"),
        "ws" => ss.get("wsSettings"),
        "httpupgrade" => ss.get("httpupgradeSettings"),
        _ => None,
    };
    if let Some(t) = transport {
        c.host = s(t, "host");
        c.path = s(t, "path");
        c.service_name = s(t, "serviceName");
        c.authority = s(t, "authority");
        c.mode = s(t, "mode");
        if t.get("multiMode").and_then(Value::as_bool) == Some(true) {
            c.mode = "multi".into();
        }
        c.extra = t.get("extra").cloned();
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REALITY_TCP: &str = "vless://b831381d-6324-4d53-ad4f-8cda48b30811@203.0.113.7:443?encryption=none&flow=xtls-rprx-vision&security=reality&sni=www.microsoft.com&fp=chrome&pbk=Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw&sid=6ba85179e30d4fc2&spx=%2F&type=tcp&headerType=none#%F0%9F%87%B3%F0%9F%87%B1%20Amsterdam";

    #[test]
    fn parses_reality_vision() {
        let (name, c) = parse_vless(REALITY_TCP).unwrap();
        assert_eq!(name, "🇳🇱 Amsterdam");
        assert_eq!(c.address, "203.0.113.7");
        assert_eq!(c.port, 443);
        assert_eq!(c.uuid, "b831381d-6324-4d53-ad4f-8cda48b30811");
        assert_eq!(c.flow, "xtls-rprx-vision");
        assert_eq!(c.security, "reality");
        assert_eq!(c.sni, "www.microsoft.com");
        assert_eq!(c.fingerprint, "chrome");
        assert_eq!(c.public_key, "Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw");
        assert_eq!(c.short_id, "6ba85179e30d4fc2");
        assert_eq!(c.spider_x, "/");
        assert_eq!(c.network, "tcp");
    }

    #[test]
    fn parses_reality_xhttp_with_extra() {
        let link = "vless://id-1@example.com:8443?security=reality&type=xhttp&path=%2Fapi%2Bv1&mode=auto&host=cdn.example.com&sni=a.example&pbk=KEY&sid=ab&extra=%7B%22xmux%22%3A%7B%22maxConcurrency%22%3A%2216-32%22%7D%7D#Stockholm";
        let (name, c) = parse_vless(link).unwrap();
        assert_eq!(name, "Stockholm");
        assert_eq!(c.network, "xhttp");
        assert_eq!(c.path, "/api+v1");
        assert_eq!(c.mode, "auto");
        assert_eq!(c.host, "cdn.example.com");
        assert_eq!(c.extra.unwrap()["xmux"]["maxConcurrency"], "16-32");
    }

    #[test]
    fn parses_grpc_ipv6_and_default_name() {
        let link = "vless://uuid@[2001:db8::1]:2053?security=tls&type=grpc&serviceName=svc&mode=multi&alpn=h2,http/1.1";
        let (name, c) = parse_vless(link).unwrap();
        assert_eq!(c.address, "2001:db8::1");
        assert_eq!(name, "2001:db8::1:2053");
        assert_eq!(c.service_name, "svc");
        assert_eq!(c.mode, "multi");
        assert_eq!(c.alpn, vec!["h2", "http/1.1"]);
    }

    #[test]
    fn rejects_broken_links() {
        assert_eq!(parse_vless("vmess://abc"), Err(LinkError::UnsupportedScheme));
        assert!(parse_vless("vless://host:443").is_err());
        assert!(parse_vless("vless://uuid@host").is_err());
        assert!(parse_vless("vless://uuid@host:99999").is_err());
        assert!(parse_vless("vless://@host:443").is_err());
    }

    #[test]
    fn link_roundtrip() {
        let (name, c) = parse_vless(REALITY_TCP).unwrap();
        let (name2, c2) = parse_vless(&to_vless_link(&name, &c)).unwrap();
        assert_eq!(name, name2);
        assert_eq!(c, c2);
    }

    #[test]
    fn converts_xray_outbound() {
        let ob: Value = serde_json::from_str(
            r#"{"protocol":"vless","tag":"proxy","settings":{"vnext":[{"address":"1.2.3.4","port":443,
            "users":[{"id":"u","encryption":"none","flow":"xtls-rprx-vision"}]}]},
            "streamSettings":{"network":"raw","security":"reality","realitySettings":{"serverName":"s",
            "fingerprint":"chrome","publicKey":"pk","shortId":"01"}}}"#,
        )
        .unwrap();
        let c = from_xray_outbound(&ob).unwrap();
        assert_eq!(c.network, "tcp");
        assert_eq!(c.security, "reality");
        assert_eq!(c.public_key, "pk");
        assert_eq!(c.flow, "xtls-rprx-vision");
    }
}
