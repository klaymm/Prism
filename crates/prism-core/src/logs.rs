use crate::models::now_millis;
use parking_lot::Mutex;
use std::collections::VecDeque;
use crate::events::{Event, EventSink};

const CAPACITY: usize = 3000;

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub id: u64,
    pub ts: i64,
    pub source: &'static str,
    pub level: &'static str,
    pub message: String,
}

#[derive(Default)]
struct Inner {
    entries: VecDeque<LogEntry>,
    pending: Vec<LogEntry>,
    next_id: u64,
}

pub struct LogBuffer {
    inner: Mutex<Inner>,
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn normalize_level(word: &str) -> Option<&'static str> {
    Some(match word.to_ascii_lowercase().as_str() {
        "error" | "fatal" | "panic" => "error",
        "warning" | "warn" => "warning",
        "info" => "info",
        "debug" | "trace" => "debug",
        _ => return None,
    })
}

pub fn parse_line(line: &str) -> (&'static str, String) {
    let (level, msg) = parse_raw(line);
    if level == "warning" && msg.starts_with("core: Xray") && msg.ends_with("started") {
        return ("info", msg);
    }
    (level, msg)
}

fn parse_raw(line: &str) -> (&'static str, String) {
    let line = strip_ansi(line.trim());
    if let (Some(start), Some(end)) = (line.find('['), line.find(']')) {
        if start < end {
            if let Some(level) = normalize_level(&line[start + 1..end]) {
                return (level, line[end + 1..].trim().to_string());
            }
        }
    }
    let word_end = line.find([' ', '[']).unwrap_or(line.len());
    if let Some(level) = normalize_level(&line[..word_end]) {
        let rest = line[word_end..].trim_start();
        let rest = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((_, msg)) => msg.trim(),
            None => rest,
        };
        return (level, rest.to_string());
    }
    ("info", line)
}

impl LogBuffer {
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner::default()) }
    }

    pub fn push(&self, source: &'static str, level: &'static str, message: impl Into<String>) {
        let mut inner = self.inner.lock();
        inner.next_id += 1;
        let entry = LogEntry { id: inner.next_id, ts: now_millis(), source, level, message: message.into() };
        if inner.entries.len() >= CAPACITY {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry.clone());
        inner.pending.push(entry);
    }

    pub fn info(&self, message: impl Into<String>) {
        self.push("app", "info", message);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.push("app", "error", message);
    }

    pub fn all(&self) -> Vec<LogEntry> {
        self.inner.lock().entries.iter().cloned().collect()
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.entries.clear();
        inner.pending.clear();
    }

    pub fn last_id(&self) -> u64 {
        self.inner.lock().next_id
    }

    pub fn find_since(&self, after_id: u64, source: &str, pred: impl Fn(&LogEntry) -> bool) -> Option<LogEntry> {
        self.inner
            .lock()
            .entries
            .iter()
            .rev()
            .take_while(|e| e.id > after_id)
            .find(|e| e.source == source && pred(e))
            .cloned()
    }

    pub fn last_error_since(&self, after_id: u64, source: &str) -> Option<String> {
        self.find_since(after_id, source, |e| e.level == "error").map(|e| e.message)
    }

    pub fn flush(&self, sink: &dyn EventSink) {
        let batch = std::mem::take(&mut self.inner.lock().pending);
        if !batch.is_empty() {
            sink.emit(Event::Logs(batch));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xray_lines() {
        let (lvl, msg) = parse_line("2026/09/27 14:47:56.780010 [Error] transport/internet/reality: REALITY: bad");
        assert_eq!(lvl, "error");
        assert_eq!(msg, "transport/internet/reality: REALITY: bad");
        let (lvl, _) = parse_line("2026/09/27 14:47:56.780010 [Warning] core: Xray 26.3.27 started");
        assert_eq!(lvl, "info");
        let (lvl, _) = parse_line("2026/09/27 14:47:56.780010 [Warning] app/dns: slow response");
        assert_eq!(lvl, "warning");
    }

    #[test]
    fn parses_singbox_lines() {
        let (lvl, msg) = parse_line("\u{1b}[36mINFO\u{1b}[0m sing-box started (0.52s)");
        assert_eq!(lvl, "info");
        assert_eq!(msg, "sing-box started (0.52s)");
        let (lvl, msg) = parse_line("ERROR [3791512954 1.2s] inbound/tun[tun-in]: connection reset");
        assert_eq!(lvl, "error");
        assert_eq!(msg, "inbound/tun[tun-in]: connection reset");
        let (lvl, msg) = parse_line("FATAL[0000] start service: start inbound/tun[tun-in]: configure tun interface: x");
        assert_eq!(lvl, "error");
        assert_eq!(msg, "start service: start inbound/tun[tun-in]: configure tun interface: x");
    }

    #[test]
    fn plain_lines_are_info() {
        assert_eq!(parse_line("A unified platform for anti-censorship.").0, "info");
    }
}
