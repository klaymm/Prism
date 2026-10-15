use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
pub const DASH: &str = "—";

fn fixed(v: f64, digits: usize) -> String {
    format!("{v:.digits$}").replace('.', ",")
}

pub fn bytes(bytes: f64) -> String {
    bytes_with(bytes, 1)
}

pub fn bytes_with(bytes: f64, digits: usize) -> String {
    if !bytes.is_finite() {
        return DASH.into();
    }
    let mut v = bytes.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    let d = if i == 0 || v >= 100.0 { 0 } else { digits };
    format!("{} {}", fixed(v, d), UNITS[i])
}

pub fn rate(bytes_per_sec: f64) -> String {
    format!("{}/с", bytes(bytes_per_sec))
}

pub fn mbps(mbps: Option<f64>) -> String {
    match mbps {
        Some(v) if v.is_finite() => fixed(v, if v >= 100.0 { 0 } else { 1 }),
        _ => DASH.into(),
    }
}

pub fn duration(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
}

const MONTHS_LONG: [&str; 12] = [
    "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря",
];
const MONTHS_SHORT: [&str; 12] =
    ["янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.", "нояб.", "дек."];

fn local(unix_secs: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(unix_secs, 0).single()
}

pub fn date(unix_secs: Option<i64>) -> String {
    match unix_secs.filter(|s| *s != 0).and_then(local) {
        Some(t) => format!("{} {} {} г.", t.day(), MONTHS_LONG[t.month0() as usize], t.year()),
        None => DASH.into(),
    }
}

pub fn date_time(unix_secs: Option<i64>) -> String {
    match unix_secs.filter(|s| *s != 0).and_then(local) {
        Some(t) => format!("{} {}, {:02}:{:02}", t.day(), MONTHS_SHORT[t.month0() as usize], t.hour(), t.minute()),
        None => DASH.into(),
    }
}

pub fn clock(unix_millis: i64) -> String {
    match Local.timestamp_millis_opt(unix_millis).single() {
        Some(t) => format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second()),
        None => String::new(),
    }
}

pub fn now_secs() -> i64 {
    Local::now().timestamp()
}

pub fn now_millis() -> i64 {
    Local::now().timestamp_millis()
}

pub fn relative(unix_secs: Option<i64>) -> String {
    relative_at(unix_secs, now_secs())
}

fn relative_at(unix_secs: Option<i64>, now: i64) -> String {
    let Some(t) = unix_secs.filter(|s| *s != 0) else { return "никогда".into() };
    let diff = (now - t).max(0);
    match diff {
        0..60 => "только что".into(),
        60..3600 => format!("{} мин назад", diff / 60),
        3600..86400 => format!("{} ч назад", diff / 3600),
        _ => format!("{} дн назад", diff / 86400),
    }
}

pub fn days_left(unix_secs: Option<i64>) -> Option<i64> {
    days_left_at(unix_secs, now_millis())
}

fn days_left_at(unix_secs: Option<i64>, now_ms: i64) -> Option<i64> {
    let t = unix_secs.filter(|s| *s != 0)?;
    Some(((t * 1000 - now_ms) as f64 / 86_400_000.0).ceil() as i64)
}

pub fn plural<'a>(n: i64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let (m10, m100) = (n.rem_euclid(10), n.rem_euclid(100));
    if m10 == 1 && m100 != 11 {
        one
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        few
    } else {
        many
    }
}

const REGIONAL_A: u32 = 0x1F1E6;

pub fn split_flag(name: &str) -> (Option<String>, String) {
    let chars: Vec<char> = name.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        let a = (chars[i] as u32).wrapping_sub(REGIONAL_A);
        let b = (chars[i + 1] as u32).wrapping_sub(REGIONAL_A);
        if a < 26 && b < 26 {
            let code: String = [a, b].iter().map(|c| char::from(b'a' + *c as u8)).collect();
            let rest: String = chars[..i].iter().chain(&chars[i + 2..]).collect();
            let label = rest.split_whitespace().collect::<Vec<_>>().join(" ");
            let label = if label.is_empty() { code.to_uppercase() } else { label };
            return (Some(code), label);
        }
    }
    (None, name.trim().to_string())
}

pub fn transport_label(security: &str, network: &str) -> String {
    let sec = match security {
        "reality" => "Reality",
        "tls" => "TLS",
        _ => "",
    };
    let net = match network {
        "tcp" | "raw" => "TCP".to_string(),
        "xhttp" => "XHTTP".to_string(),
        other => other.to_uppercase(),
    };
    [sec, net.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    None,
    Good,
    Fair,
    Poor,
    Fail,
}

pub fn latency_tone(ms: Option<i64>) -> Tone {
    match ms {
        None => Tone::None,
        Some(ms) if ms < 0 => Tone::Fail,
        Some(ms) if ms < 150 => Tone::Good,
        Some(ms) if ms < 350 => Tone::Fair,
        Some(_) => Tone::Poor,
    }
}

pub fn nice_max(peak: f64) -> f64 {
    let mut v = 64.0 * 1024.0;
    while v < peak * 1.1 {
        v *= 2.0;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_rates() {
        assert_eq!(bytes(0.0), "0 Б");
        assert_eq!(bytes(1536.0), "1,5 КБ");
        assert_eq!(bytes(150.0 * 1024.0), "150 КБ");
        assert_eq!(bytes(3.6 * 1024.0 * 1024.0), "3,6 МБ");
        assert_eq!(bytes_with(246.0 * 1024f64.powi(3), 0), "246 ГБ");
        assert_eq!(rate(20172.0), "19,7 КБ/с");
        assert_eq!(bytes(f64::NAN), DASH);
        assert_eq!(mbps(Some(94.26)), "94,3");
        assert_eq!(mbps(Some(312.4)), "312");
        assert_eq!(mbps(None), DASH);
    }

    #[test]
    fn durations_and_relative_times() {
        assert_eq!(duration(40_000), "00:00:40");
        assert_eq!(duration(3_723_000), "01:02:03");
        assert_eq!(relative_at(None, 1000), "никогда");
        assert_eq!(relative_at(Some(990), 1000), "только что");
        assert_eq!(relative_at(Some(1000 - 5 * 60), 1000), "5 мин назад");
        assert_eq!(relative_at(Some(1000 - 7200), 1000), "2 ч назад");
        assert_eq!(relative_at(Some(1000 - 3 * 86400), 1000), "3 дн назад");
        assert_eq!(days_left_at(Some(86400 * 2), 86400 * 1000), Some(1));
        assert_eq!(days_left_at(Some(86400 * 2 + 1), 86400 * 1000), Some(2));
        assert_eq!(days_left_at(None, 0), None);
    }

    #[test]
    fn russian_plurals() {
        let p = |n| plural(n, "сервер", "сервера", "серверов");
        assert_eq!([p(1), p(2), p(5), p(11), p(12), p(21), p(22), p(25), p(111)], [
            "сервер", "сервера", "серверов", "серверов", "серверов", "сервер", "сервера", "серверов", "серверов"
        ]);
    }

    #[test]
    fn flag_is_split_from_name() {
        assert_eq!(split_flag("🇩🇪 Germany  Fast"), (Some("de".into()), "Germany Fast".into()));
        assert_eq!(split_flag("Frankfurt 🇩🇪"), (Some("de".into()), "Frankfurt".into()));
        assert_eq!(split_flag("🇳🇱"), (Some("nl".into()), "NL".into()));
        assert_eq!(split_flag("  plain  "), (None, "plain".into()));
    }

    #[test]
    fn transport_and_latency() {
        assert_eq!(transport_label("reality", "tcp"), "Reality · TCP");
        assert_eq!(transport_label("tls", "xhttp"), "TLS · XHTTP");
        assert_eq!(transport_label("none", "grpc"), "GRPC");
        assert_eq!(latency_tone(Some(-1)), Tone::Fail);
        assert_eq!(latency_tone(Some(149)), Tone::Good);
        assert_eq!(latency_tone(Some(349)), Tone::Fair);
        assert_eq!(latency_tone(Some(350)), Tone::Poor);
        assert_eq!(nice_max(0.0), 65536.0);
        assert_eq!(nice_max(3.0e6), 4.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn dates_in_russian() {
        let t = Local.with_ymd_and_hms(2026, 10, 12, 14, 5, 0).unwrap().timestamp();
        assert_eq!(date(Some(t)), "12 октября 2026 г.");
        assert_eq!(date_time(Some(t)), "12 окт., 14:05");
        assert_eq!(date(None), DASH);
    }
}
