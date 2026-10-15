use crate::models::{Server, Subscription, VlessConfig, now_secs};
use crate::share_link::{self, LinkError};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use reqwest::header::HeaderMap;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct ParsedBody {
    pub servers: Vec<(String, String, VlessConfig)>,
    pub skipped: usize,
}

#[derive(Debug, Default, PartialEq)]
pub struct SubscriptionMeta {
    pub title: Option<String>,
    pub update_interval_hours: Option<u32>,
    pub upload: Option<u64>,
    pub download: Option<u64>,
    pub total: Option<u64>,
    pub expire: Option<i64>,
    pub support_url: Option<String>,
}

pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(cleaned.as_bytes()).ok())
}

pub fn parse_body(body: &str) -> ParsedBody {
    let body = body.trim().trim_start_matches('\u{feff}');
    let mut out = ParsedBody::default();

    if body.starts_with('[') || body.starts_with('{') {
        if let Ok(json) = serde_json::from_str::<Value>(body) {
            parse_json_configs(&json, &mut out);
            return out;
        }
    }

    let text = if body.lines().any(|l| l.contains("://")) {
        body.to_string()
    } else {
        match decode_base64(body).and_then(|b| String::from_utf8(b).ok()) {
            Some(t) => t,
            None => return out,
        }
    };

    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match share_link::parse_vless(line) {
            Ok((name, cfg)) => out.servers.push((name, line.to_string(), cfg)),
            Err(LinkError::UnsupportedScheme) if !share_link::is_share_link(line) => {}
            Err(_) => out.skipped += 1,
        }
    }
    out
}

fn parse_json_configs(json: &Value, out: &mut ParsedBody) {
    let configs: Vec<&Value> = match json {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    for cfg in configs {
        let name = cfg.get("remarks").and_then(Value::as_str).unwrap_or_default();
        let outbounds = cfg.get("outbounds").and_then(Value::as_array);
        let vless = outbounds.and_then(|obs| obs.iter().find_map(share_link::from_xray_outbound));
        match vless {
            Some(c) => {
                let name = if name.is_empty() { format!("{}:{}", c.address, c.port) } else { name.to_string() };
                let link = share_link::to_vless_link(&name, &c);
                out.servers.push((name, link, c));
            }
            None => out.skipped += 1,
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|s| !s.is_empty())
}

fn decode_title(raw: &str) -> String {
    match raw.strip_prefix("base64:") {
        Some(b64) => decode_base64(b64)
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_else(|| raw.to_string()),
        None => raw.to_string(),
    }
}

pub fn parse_meta(headers: &HeaderMap) -> SubscriptionMeta {
    let mut meta = SubscriptionMeta {
        title: header(headers, "profile-title").map(decode_title),
        update_interval_hours: header(headers, "profile-update-interval").and_then(|v| v.parse().ok()),
        support_url: header(headers, "support-url").map(String::from),
        ..Default::default()
    };
    if let Some(info) = header(headers, "subscription-userinfo") {
        for part in info.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let num = v.trim().parse::<f64>().ok().filter(|n| *n >= 0.0);
            match k.trim() {
                "upload" => meta.upload = num.map(|n| n as u64),
                "download" => meta.download = num.map(|n| n as u64),
                "total" => meta.total = num.map(|n| n as u64).filter(|n| *n > 0),
                "expire" => meta.expire = num.map(|n| n as i64).filter(|n| *n > 0),
                _ => {}
            }
        }
    }
    meta
}

pub struct Fetched {
    pub meta: SubscriptionMeta,
    pub parsed: ParsedBody,
}

pub async fn fetch(client: &reqwest::Client, url: &str, user_agent: &str) -> Result<Fetched, String> {
    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, user_agent)
        .send()
        .await
        .map_err(|e| format!("не удалось загрузить подписку: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("сервер подписки ответил {}", resp.status()));
    }
    let meta = parse_meta(resp.headers());
    let body = resp.text().await.map_err(|e| format!("ошибка чтения подписки: {e}"))?;
    let parsed = parse_body(&body);
    if parsed.servers.is_empty() {
        return Err("в подписке не найдено серверов VLESS".into());
    }
    Ok(Fetched { meta, parsed })
}

pub fn apply_meta(sub: &mut Subscription, meta: SubscriptionMeta) {
    if let Some(t) = meta.title {
        sub.name = t;
    }
    sub.update_interval_hours = meta.update_interval_hours;
    sub.upload = meta.upload;
    sub.download = meta.download;
    sub.total = meta.total;
    sub.expire = meta.expire;
    sub.support_url = meta.support_url;
    sub.updated_at = Some(now_secs());
    sub.last_error = None;
}

pub fn merge_servers(existing: &[Server], sub_id: &str, parsed: Vec<(String, String, VlessConfig)>) -> Vec<Server> {
    parsed
        .into_iter()
        .map(|(name, link, config)| {
            let prev = existing
                .iter()
                .find(|s| s.subscription_id.as_deref() == Some(sub_id) && (s.link == link || s.config == config));
            Server {
                id: prev.map(|p| p.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                subscription_id: Some(sub_id.to_string()),
                name,
                link,
                config,
                latency: prev.and_then(|p| p.latency.clone()),
            }
        })
        .collect()
}

pub fn host_title(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_else(|| "Подписка".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    const L1: &str = "vless://u1@1.1.1.1:443?security=reality&type=tcp&pbk=k&sid=1#A";
    const L2: &str = "vless://u2@2.2.2.2:443?security=reality&type=xhttp&pbk=k&sid=2#B";

    #[test]
    fn plain_list() {
        let p = parse_body(&format!("{L1}\n\n{L2}\n"));
        assert_eq!(p.servers.len(), 2);
        assert_eq!(p.skipped, 0);
    }

    #[test]
    fn base64_variants() {
        let text = format!("{L1}\r\n{L2}\r\nvmess://eyJ2IjoiMiJ9\r\n");
        for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
            let encoded = engine.encode(text.as_bytes());
            let wrapped: String = encoded
                .as_bytes()
                .chunks(76)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let p = parse_body(&wrapped);
            assert_eq!(p.servers.len(), 2, "engine failed");
            assert_eq!(p.skipped, 1);
        }
    }

    #[test]
    fn xray_json_array() {
        let body = r#"[{"remarks":"NL","outbounds":[{"protocol":"vless","settings":{"vnext":[{"address":"1.2.3.4","port":443,"users":[{"id":"u"}]}]},"streamSettings":{"network":"tcp","security":"reality","realitySettings":{"publicKey":"pk"}}},{"protocol":"freedom"}]}]"#;
        let p = parse_body(body);
        assert_eq!(p.servers.len(), 1);
        assert_eq!(p.servers[0].0, "NL");
        assert!(p.servers[0].1.starts_with("vless://u@1.2.3.4:443"));
    }

    #[test]
    fn garbage_yields_nothing() {
        let p = parse_body("<html>not found</html>");
        assert!(p.servers.is_empty());
    }

    #[test]
    fn meta_headers() {
        let mut h = HeaderMap::new();
        let mut put = |k: &'static str, v: &str| {
            h.insert(HeaderName::from_static(k), HeaderValue::from_str(v).unwrap());
        };
        put("subscription-userinfo", "upload=100; download=2500.0; total=107374182400; expire=1800000000");
        put("profile-title", &format!("base64:{}", STANDARD.encode("Мой VPN")));
        put("profile-update-interval", "12");
        put("support-url", "https://t.me/support");
        let m = parse_meta(&h);
        assert_eq!(m.title.as_deref(), Some("Мой VPN"));
        assert_eq!(m.update_interval_hours, Some(12));
        assert_eq!(m.upload, Some(100));
        assert_eq!(m.download, Some(2500));
        assert_eq!(m.total, Some(107_374_182_400));
        assert_eq!(m.expire, Some(1_800_000_000));
        assert_eq!(m.support_url.as_deref(), Some("https://t.me/support"));
    }

    #[test]
    fn zero_total_and_expire_mean_unlimited() {
        let mut h = HeaderMap::new();
        h.insert("subscription-userinfo", HeaderValue::from_static("upload=0; download=0; total=0; expire=0"));
        let m = parse_meta(&h);
        assert_eq!(m.total, None);
        assert_eq!(m.expire, None);
    }

    #[test]
    fn merge_keeps_ids() {
        let first = merge_servers(&[], "s", parse_body(L1).servers);
        let second = merge_servers(&first, "s", parse_body(&format!("{L1}\n{L2}")).servers);
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].id, first[0].id);
        assert_ne!(second[1].id, first[0].id);
    }
}
