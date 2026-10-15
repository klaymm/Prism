use crate::{App, AppWindow, ChartHover, LogRow, Option as Opt, Page, PrismTray, RuleTag, SegmentItem, ServerRow, SpeedRow, SubInfo, Theme, ToastData, TrayServer};
use crate::{chart, flags};
use prism_core::events::Event;
use prism_core::logs::LogEntry;
use prism_core::models::{ConnStatus, ConnectionInfo, LatencyKind, RoutingMode, Server, ServerStore, Settings, SpeedResult, Subscription, display_name};
use prism_core::routing::{DnsServer, RoutingState};
use prism_core::rules;
use prism_core::service::{ELEVATION_TEXT, Traffic};
use prism_core::speedtest::SpeedProgress;
use prism_core::xray_config;
use prism_core::state::Shared;
use prism_core::updater::UpdateStatus;
use prism_ui::format::{self, Tone};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

const MAX_LOGS: usize = 3000;
const RENDER_LOGS: usize = 1000;
const MAX_TRAY_SERVERS: usize = 30;
pub const LIVE_TARGET: &str = "__live";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success = 0,
    Error = 1,
}

pub struct View {
    pub win: AppWindow,
    pub tray: PrismTray,
    pub state: Shared,
    pub settings: Settings,
    pub store: ServerStore,
    pub conn: ConnectionInfo,
    routing: RoutingState,
    speed_history: Vec<SpeedResult>,
    logs: VecDeque<LogEntry>,
    logs_dirty: bool,
    traffic: Option<Traffic>,
    toasts: Rc<VecModel<ToastData>>,
    toast_seq: i32,
    pub pinging: bool,
    speed: Option<SpeedProgress>,
    pub speed_running: bool,
    pub speed_target: Option<String>,
    server_filter: (String, String, String),
    log_filter: (String, String, String),
    pub visible_servers: Vec<String>,
    busy_subs: HashSet<String>,
    update: UpdateStatus,
    notified_update: Option<String>,
    ticker: slint::Timer,
}

thread_local!(static VIEW: RefCell<Option<View>> = const { RefCell::new(None) });

pub fn with<R>(f: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEW.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn init(win: AppWindow, tray: PrismTray, state: Shared, speed_history: Vec<SpeedResult>) {
    let snap = prism_core::api::snapshot(&state);
    let toasts = Rc::new(VecModel::default());
    win.global::<App>().set_toasts(ModelRc::from(toasts.clone()));
    let logs: VecDeque<LogEntry> = state.logs.all().into();
    let view = View {
        win,
        tray,
        settings: snap.settings,
        store: snap.store,
        conn: snap.conn,
        routing: snap.routing,
        speed_history,
        logs,
        logs_dirty: true,
        traffic: None,
        toasts,
        toast_seq: 0,
        pinging: false,
        speed: None,
        speed_running: false,
        speed_target: None,
        server_filter: ("all".into(), String::new(), "default".into()),
        log_filter: ("all".into(), "all".into(), String::new()),
        visible_servers: Vec::new(),
        busy_subs: HashSet::new(),
        update: UpdateStatus::default(),
        notified_update: None,
        ticker: slint::Timer::default(),
        state,
    };
    VIEW.set(Some(view));
    with(|v| {
        let app = v.app();
        app.set_version(env!("CARGO_PKG_VERSION").into());
        app.set_elevated(v.state.elevated);
        app.set_data_dir(v.state.paths.data.display().to_string().into());
        v.push_versions();
        v.push_settings();
        v.push_conn();
        v.push_servers();
        v.push_routing();
        v.push_speed();
        v.push_traffic();
        v.ticker.start(slint::TimerMode::Repeated, Duration::from_secs(1), || {
            with(|v| v.push_duration());
        });
    });
}

pub fn shutdown() {
    VIEW.set(None);
}

pub fn apply(event: Event) {
    with(|v| match event {
        Event::Conn(c) => v.on_conn(c),
        Event::Settings(s) => {
            let ru_direct_changed = v.settings.ru_direct != s.ru_direct;
            v.settings = s;
            v.push_settings();
            v.push_servers();
            if ru_direct_changed {
                v.push_routing();
            }
        }
        Event::Store(s) => {
            v.store = s;
            v.push_servers();
        }
        Event::Routing(r) => {
            v.routing = r;
            v.push_routing();
        }
        Event::Traffic(t) => v.on_traffic(t),
        Event::Logs(batch) => v.on_logs(batch),
        Event::Latency { server_id, latency } => {
            if let Some(s) = v.store.servers.iter_mut().find(|s| s.id == server_id) {
                s.latency = Some(latency);
            }
            v.push_servers();
        }
        Event::Speed(p) => {
            v.speed = Some(p);
            v.push_speed();
        }
        Event::Update(s) => v.on_update(s),
    });
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn sh(text: impl AsRef<str>) -> SharedString {
    text.as_ref().into()
}

pub fn import_message(r: &prism_core::data::ImportResult) -> String {
    let n = r.added as i64;
    let servers = format!("{n} {}", format::plural(n, "сервер", "сервера", "серверов"));
    let head = match &r.name {
        Some(name) => format!("«{name}»: {servers}"),
        None => format!("Добавлено: {servers}"),
    };
    if r.skipped > 0 { format!("{head}, пропущено {} (неподдерживаемые)", r.skipped) } else { head }
}

fn tone_index(ms: Option<i64>) -> i32 {
    match format::latency_tone(ms) {
        Tone::None => 0,
        Tone::Good => 1,
        Tone::Fair => 2,
        Tone::Poor | Tone::Fail => 3,
    }
}

fn server_row(s: &Server, selected: bool, live: bool) -> ServerRow {
    let (code, label) = format::split_flag(&s.name);
    let latency = s.latency.as_ref();
    ServerRow {
        id: sh(&s.id),
        label: sh(label),
        flag: flags::image(code.as_deref()),
        transport: sh(format::transport_label(&s.config.security, &s.config.network)),
        latency: match latency {
            None => sh(format::DASH),
            Some(l) if l.ms < 0 => sh("нет ответа"),
            Some(l) => sh(format!("{} мс", l.ms)),
        },
        latency_tone: tone_index(latency.map(|l| l.ms)),
        latency_tip: sh(match latency.map(|l| l.kind) {
            None => "Задержка не измерялась",
            Some(LatencyKind::Real) => "Реальная задержка через сервер",
            Some(LatencyKind::Tcp) => "TCP-пинг до сервера",
        }),
        selected,
        live,
        manual: s.subscription_id.is_none(),
    }
}

fn sub_info(sub: &Subscription) -> SubInfo {
    let used = sub.upload.unwrap_or(0) + sub.download.unwrap_or(0);
    let total = sub.total.filter(|t| *t > 0);
    let days = format::days_left(sub.expire);
    SubInfo {
        id: sh(&sub.id),
        name: sh(&sub.name),
        used: sh(format::bytes(used as f64)),
        total: sh(total.map_or("∞".to_string(), |t| format::bytes(t as f64))),
        has_total: total.is_some(),
        ratio: total.map_or(0.0, |t| (used as f64 / t as f64).min(1.0)) as f32,
        has_expire: sub.expire.is_some_and(|e| e != 0),
        expire: sh(format!("до {}", format::date(sub.expire))),
        days: sh(match days {
            Some(d) if d >= 0 => format!(" · {d} {}", format::plural(d, "день", "дня", "дней")),
            _ => " · истекла".to_string(),
        }),
        warn: days.is_some_and(|d| d <= 3),
        updated: sh(format!("обновлена {}", format::relative(sub.updated_at))),
        error: sh(sub.last_error.clone().unwrap_or_default()),
        has_support: sub.support_url.is_some(),
    }
}

const TAG_NAMES: &[(&str, &str)] = &[
    ("geosite:private", "Локальные адреса"),
    ("geosite:category-ru", "Российские сайты"),
    ("geosite:whitelist", "Белый список РФ"),
    ("geosite:microsoft", "Microsoft"),
    ("geosite:apple", "Apple"),
    ("geosite:epicgames", "Epic Games"),
    ("geosite:riot", "Riot Games"),
    ("geosite:escapefromtarkov", "Escape from Tarkov"),
    ("geosite:steam", "Steam"),
    ("geosite:twitch", "Twitch"),
    ("geosite:pinterest", "Pinterest"),
    ("geosite:faceit", "FACEIT"),
    ("geosite:google-play", "Google Play"),
    ("geosite:github", "GitHub"),
    ("geosite:twitch-ads", "Реклама Twitch"),
    ("geosite:youtube", "YouTube"),
    ("geosite:telegram", "Telegram"),
    ("geosite:win-spy", "Телеметрия Windows"),
    ("geosite:torrent", "Торрент-трекеры"),
    ("geosite:category-ads", "Реклама"),
    ("geoip:private", "Локальная сеть"),
    ("geoip:direct", "IP-адреса РФ"),
    ("geoip:whitelist", "Облака РФ"),
];

fn ru_direct_tags() -> [RuleTag; 2] {
    [
        RuleTag { label: sh("Зоны .ru, .su, .рф"), tag: sh(xray_config::RU_ZONES.join(", ")) },
        RuleTag { label: sh("Облака РФ"), tag: sh(xray_config::RU_CLOUD_IP) },
    ]
}

fn rule_tags(sites: &[String], ips: &[String]) -> Vec<RuleTag> {
    let tags = sites.iter().chain(ips).map(|tag| {
        let label = TAG_NAMES.iter().find(|(t, _)| t == tag).map(|(_, n)| n.to_string()).unwrap_or_else(|| {
            tag.strip_prefix("geosite:").or_else(|| tag.strip_prefix("geoip:")).unwrap_or(tag).to_string()
        });
        RuleTag { label: sh(label), tag: sh(tag) }
    });
    tags.collect()
}

fn rule_chips(entries: &[String]) -> Vec<RuleTag> {
    entries.iter().map(|e| RuleTag { label: sh(rules::display(e)), tag: sh(e) }).collect()
}

fn dns_label(d: &DnsServer) -> String {
    if !d.doh.is_empty() {
        format!("DoH · {}", d.doh.trim_start_matches("https://"))
    } else if !d.ip.is_empty() {
        d.ip.clone()
    } else {
        format::DASH.into()
    }
}

fn name_key(label: &str) -> (u8, String) {
    let first = label.chars().next().unwrap_or(' ');
    let rank = if !first.is_alphabetic() {
        0
    } else if ('\u{400}'..='\u{4FF}').contains(&first) {
        1
    } else if first.is_ascii_alphabetic() {
        2
    } else {
        3
    };
    (rank, label.to_lowercase())
}

fn status_name(status: ConnStatus) -> &'static str {
    match status {
        ConnStatus::Connected => "connected",
        ConnStatus::Connecting => "connecting",
        ConnStatus::Disconnecting => "disconnecting",
        ConnStatus::Error => "error",
        ConnStatus::Disconnected => "disconnected",
    }
}

fn log_line(l: &LogEntry) -> String {
    format!("{} [{}] {} {}", format::clock(l.ts), l.source, level_short(l.level), l.message)
}

fn level_short(level: &str) -> &'static str {
    match level {
        "error" => "ERR",
        "warning" => "WRN",
        "debug" => "DBG",
        _ => "INF",
    }
}

impl View {
    pub fn app(&self) -> App<'_> {
        self.win.global::<App>()
    }

    fn live_id(&self) -> Option<&str> {
        matches!(self.conn.status, ConnStatus::Connected | ConnStatus::Connecting).then_some(()).and(self.conn.server_id.as_deref())
    }

    pub fn current_server(&self) -> Option<&Server> {
        let id = self.live_id().or(self.settings.selected_server.as_deref());
        self.store.servers.iter().find(|s| Some(s.id.as_str()) == id).or(self.store.servers.first())
    }

    pub fn is_connected(&self) -> bool {
        self.conn.status == ConnStatus::Connected
    }

    pub fn is_active(&self) -> bool {
        matches!(self.conn.status, ConnStatus::Connected | ConnStatus::Connecting)
    }

    // Toasts

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        while self.toasts.row_count() >= 4 {
            self.toasts.remove(0);
        }
        self.toasts.push(ToastData { id, kind: kind as i32, text: sh(text.into()) });
        let ms = if kind == ToastKind::Error { 6000 } else { 3500 };
        slint::Timer::single_shot(Duration::from_millis(ms), move || {
            with(|v| v.dismiss_toast(id));
        });
    }

    pub fn dismiss_toast(&mut self, id: i32) {
        if let Some(i) = self.toasts.iter().position(|t| t.id == id) {
            self.toasts.remove(i);
        }
    }

    pub fn fail(&mut self, error: String) {
        if error == "elevation-required" {
            self.app().set_elevation_open(true);
        } else {
            self.toast(ToastKind::Error, error);
        }
    }

    // Events

    fn on_conn(&mut self, next: ConnectionInfo) {
        let prev = self.conn.status;
        let reset = (next.status != ConnStatus::Connected && prev == ConnStatus::Connected)
            || next.status == ConnStatus::Connecting;
        let failed = next.status == ConnStatus::Error && prev != ConnStatus::Error;
        let error = next.error.clone();
        self.conn = next;
        if reset {
            self.traffic = None;
            chart::clear();
            self.push_traffic();
        }
        if failed {
            match error {
                Some(e) if e == ELEVATION_TEXT => self.app().set_elevation_open(true),
                Some(e) => self.toast(ToastKind::Error, e),
                None => {}
            }
        }
        self.push_conn();
        self.push_servers();
    }

    fn on_traffic(&mut self, t: Traffic) {
        chart::push(format::now_millis(), t.up, t.down);
        self.traffic = Some(t);
        self.push_traffic();
    }

    fn on_logs(&mut self, batch: Vec<LogEntry>) {
        let last = self.logs.back().map(|l| l.id);
        self.logs.extend(batch.into_iter().filter(|l| last.is_none_or(|id| l.id > id)));
        while self.logs.len() > MAX_LOGS {
            self.logs.pop_front();
        }
        self.logs_dirty = true;
        if self.app().get_page() == Page::Logs {
            self.push_logs();
        }
    }

    // Push

    fn push_versions(&self) {
        let v = prism_core::api::core_versions(&self.state);
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or(format::DASH).to_string();
        self.app().set_cores_text(sh(format!("Xray {} · sing-box {}", get("xray"), get("singbox"))));
    }

    fn push_settings(&self) {
        let st = &self.settings;
        let app = self.app();
        self.win.global::<Theme>().set_mode(sh(&st.theme));
        app.set_theme(sh(&st.theme));
        app.set_routing_mode(sh(match st.routing_mode {
            RoutingMode::Smart => "smart",
            RoutingMode::Global => "global",
        }));
        app.set_ru_direct(st.ru_direct);
        app.set_proxy_rules(model(rule_chips(&st.proxy_rules)));
        app.set_direct_rules(model(rule_chips(&st.direct_rules)));
        let apps = st.direct_apps.iter().map(|a| RuleTag { label: sh(a), tag: sh(a) }).collect();
        app.set_direct_apps(model(apps));
        app.set_tun(st.tun);
        app.set_autostart(st.autostart);
        app.set_connect_on_launch(st.connect_on_launch);
        app.set_close_to_tray(st.close_to_tray);
        app.set_auto_update(st.auto_update);
        app.set_socks_port(sh(st.socks_port.to_string()));
        app.set_http_port(sh(st.http_port.to_string()));
        app.set_allow_lan(st.allow_lan);
        app.set_log_level(sh(&st.log_level));
        app.set_tun_stack(sh(&st.tun_stack));
        app.set_tun_mtu(sh(st.tun_mtu.to_string()));
        app.set_user_agent(sh(&st.user_agent));
        app.set_routing_url(sh(&st.routing_url));
        app.set_routing_url_default(st.routing_url == prism_core::models::DEFAULT_ROUTING_URL);
        self.push_tray();
    }

    fn push_conn(&self) {
        let app = self.app();
        let c = &self.conn;
        app.set_status(sh(status_name(c.status)));
        app.set_conn_error(sh(c.error.clone().unwrap_or_default()));
        app.set_conn_note(sh(c.note.clone().unwrap_or_default()));
        app.set_ip(sh(c.ip.clone().unwrap_or_default()));
        app.set_ip_pending(c.ip_pending);
        app.set_ip_flag(flags::image(c.country.as_deref()));
        app.set_conn_tun(c.tun);
        self.push_duration();
        self.push_tray();
    }

    fn push_duration(&self) {
        let text = match (self.conn.status, self.conn.since) {
            (ConnStatus::Connected, Some(since)) => format::duration(format::now_millis() - since),
            _ => String::new(),
        };
        self.app().set_duration(sh(text));
    }

    fn on_update(&mut self, status: UpdateStatus) {
        if let UpdateStatus::Available(r) = &status {
            if self.notified_update.as_deref() != Some(r.version.as_str()) {
                self.notified_update = Some(r.version.clone());
                self.toast(ToastKind::Success, format!("Доступна новая версия Prism {}", r.version));
            }
        }
        self.update = status;
        self.push_update();
    }

    fn push_update(&self) {
        let app = self.app();
        let (text, version) = match &self.update {
            UpdateStatus::Unknown => ("Ещё не проверялись".to_string(), None),
            UpdateStatus::Checking => ("Проверяем…".to_string(), None),
            UpdateStatus::Latest => ("Установлена последняя версия".to_string(), None),
            UpdateStatus::Available(r) => (format!("Доступна версия {}", r.version), Some(r)),
            UpdateStatus::Downloading { release, progress } => {
                (format!("Загружаем версию {}: {} %", release.version, (progress * 100.0).round()), Some(release))
            }
            UpdateStatus::Installing(r) => (format!("Ставим версию {}, Prism перезапустится сам", r.version), Some(r)),
            UpdateStatus::Failed(e) => (e.clone(), None),
        };
        app.set_update_text(sh(text));
        app.set_update_available(version.is_some());
        app.set_update_version(sh(version.map(|r| r.version.as_str()).unwrap_or_default()));
        app.set_has_release_page(version.is_some_and(|r| !r.page_url.is_empty()));
        app.set_update_busy(matches!(
            self.update,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing(_)
        ));
    }

    pub fn chart_resized(&self, width: f32, height: f32) {
        if chart::resize(width, height) {
            self.push_traffic();
        }
    }

    fn push_traffic(&self) {
        let app = self.app();
        let t = self.traffic.as_ref();
        app.set_down_rate(sh(format::rate(t.map_or(0.0, |t| t.down))));
        app.set_up_rate(sh(format::rate(t.map_or(0.0, |t| t.up))));
        app.set_down_total(sh(format::bytes(t.map_or(0, |t| t.total_down) as f64)));
        app.set_up_total(sh(format::bytes(t.map_or(0, |t| t.total_up) as f64)));
        let p = chart::paths();
        app.set_down_line(sh(p.down_line));
        app.set_down_area(sh(p.down_area));
        app.set_up_line(sh(p.up_line));
        app.set_up_area(sh(p.up_area));
        app.set_y_ticks(model(p.ticks.iter().map(sh).collect()));
        app.set_chart_empty(p.empty);
        app.set_chart_rev(app.get_chart_rev().wrapping_add(1));
    }

    pub fn push_servers(&mut self) {
        let app = self.app();
        let live = self.live_id().map(str::to_string);
        let selected = self.settings.selected_server.clone();
        let current = self.current_server().cloned();

        app.set_has_server(current.is_some());
        if let Some(cur) = &current {
            app.set_current(server_row(cur, false, false));
        }
        let detail = match (&current, self.is_active()) {
            (Some(cur), true) => format::split_flag(&cur.name).1,
            _ => match self.conn.mode {
                RoutingMode::Smart => "Всё, кроме РФ".into(),
                RoutingMode::Global => "Глобальный".into(),
            },
        };
        app.set_sidebar_detail(sh(detail));

        let subs = &self.store.subscriptions;
        let home_sub = current
            .as_ref()
            .and_then(|c| subs.iter().find(|s| Some(&s.id) == c.subscription_id.as_ref()))
            .or(subs.first());
        app.set_has_sub(home_sub.is_some());
        if let Some(sub) = home_sub {
            app.set_home_sub(sub_info(sub));
            app.set_home_sub_busy(self.busy_subs.contains(&sub.id));
        }

        // Servers page
        let count = self.store.servers.len() as i64;
        app.set_server_count(count as i32);
        app.set_server_count_text(sh(if count > 0 {
            format!("{count} {}", format::plural(count, "сервер", "сервера", "серверов"))
        } else {
            String::new()
        }));
        let has_manual = self.store.servers.iter().any(|s| s.subscription_id.is_none());
        let mut tabs = vec![Opt { value: sh("all"), label: sh("Все") }];
        tabs.extend(subs.iter().map(|sub| Opt { value: sh(&sub.id), label: sh(&sub.name) }));
        if has_manual {
            tabs.push(Opt { value: sh("manual"), label: sh("Вручную") });
        }
        let (tab, query, sort) = self.server_filter.clone();
        let tab = if tabs.iter().any(|t| t.value == tab.as_str()) { tab } else { "all".to_string() };
        app.set_server_tab(sh(&tab));
        let items = tabs
            .iter()
            .map(|t| SegmentItem { value: t.value.clone(), label: t.label.clone(), ..Default::default() })
            .collect();
        app.set_sub_tab_items(model(items));
        app.set_sub_tabs(model(tabs));
        let active_sub = subs.iter().find(|s| s.id == tab);
        app.set_has_active_sub(active_sub.is_some());
        if let Some(sub) = active_sub {
            app.set_active_sub(sub_info(sub));
            app.set_active_sub_busy(self.busy_subs.contains(&sub.id));
        }

        let q = query.trim().to_lowercase();
        let mut list: Vec<&Server> = self
            .store
            .servers
            .iter()
            .filter(|s| match tab.as_str() {
                "all" => true,
                "manual" => s.subscription_id.is_none(),
                id => s.subscription_id.as_deref() == Some(id),
            })
            .filter(|s| q.is_empty() || s.name.to_lowercase().contains(&q) || s.config.address.to_lowercase().contains(&q))
            .collect();
        match sort.as_str() {
            "latency" => list.sort_by_key(|s| match s.latency.as_ref().map(|l| l.ms) {
                None => 1_000_000_000,
                Some(ms) if ms < 0 => 100_000_000,
                Some(ms) => ms,
            }),
            "name" => list.sort_by_cached_key(|s| name_key(&format::split_flag(&s.name).1)),
            _ => {}
        }
        let ids: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
        let rows: Vec<ServerRow> = list
            .iter()
            .map(|sv| {
                server_row(sv, selected.as_deref() == Some(sv.id.as_str()), live.as_deref() == Some(sv.id.as_str()))
            })
            .collect();
        app.set_servers(model(rows));
        app.set_pinging(self.pinging);
        self.visible_servers = ids;

        self.push_speed_targets();
        self.push_tray();
    }

    pub fn set_server_filter(&mut self, tab: String, query: String, sort: String) {
        self.server_filter = (tab, query, sort);
        self.push_servers();
    }

    pub fn set_sub_busy(&mut self, id: &str, busy: bool) {
        if busy {
            self.busy_subs.insert(id.to_string());
        } else {
            self.busy_subs.remove(id);
        }
        self.push_servers();
    }

    fn push_routing(&self) {
        let app = self.app();
        let r = &self.routing;
        let p = &r.profile;
        let name = if p.name.is_empty() { "roscomvpn" } else { p.name.as_str() };
        let updated = r.fetched_at.or(Some(p.last_updated).filter(|t| *t != 0));
        app.set_routing_meta(sh(format!("Профиль {name} · обновлён {}", format::date_time(updated))));
        app.set_routing_error(sh(r.last_error.clone().unwrap_or_default()));
        app.set_proxy_tags(model(rule_tags(&p.proxy_sites, &p.proxy_ip)));
        let mut direct = rule_tags(&p.direct_sites, &p.direct_ip);
        if self.settings.ru_direct {
            let [zones, clouds] = ru_direct_tags();
            direct.push(zones);
            let has_clouds = xray_config::has_ru_cloud_ip(&self.state.paths.geo.join("geoip.dat"));
            if has_clouds && !direct.iter().any(|t| t.tag == clouds.tag) {
                direct.push(clouds);
            }
        }
        app.set_direct_tags(model(direct));
        app.set_block_tags(model(rule_tags(&p.block_sites, &p.block_ip)));
        app.set_fallthrough(sh(format!(
            "Всё, что не попало в списки, идёт {}.",
            if p.global_proxy { "через VPN" } else { "напрямую" }
        )));
        app.set_remote_dns(sh(dns_label(&p.remote_dns)));
        app.set_domestic_dns(sh(dns_label(&p.domestic_dns)));
    }

    fn push_speed_targets(&self) {
        let app = self.app();
        let mut options = Vec::new();
        if self.is_connected() {
            options.push(Opt { value: sh(LIVE_TARGET), label: sh("Текущее подключение") });
        }
        options.extend(
            self.store.servers.iter().map(|sv| Opt { value: sh(&sv.id), label: sh(format::split_flag(&sv.name).1) }),
        );
        let value = self
            .speed_target
            .clone()
            .filter(|t| self.store.servers.iter().any(|s| &s.id == t))
            .or_else(|| options.first().map(|o| o.value.to_string()))
            .unwrap_or_else(|| LIVE_TARGET.to_string());
        app.set_speed_target(sh(value));
        app.set_speed_targets(model(options));
    }

    pub fn push_speed(&self) {
        let app = self.app();
        let p = self.speed.as_ref();
        let phase = p.map_or("", |p| p.phase);
        let step = match phase {
            "ping" => 0,
            "download" => 1,
            "upload" => 2,
            "done" => 3,
            _ => -1,
        };
        let running = self.speed_running;
        let live = (running && matches!(phase, "download" | "upload")).then(|| p.map(|p| p.mbps)).flatten();
        let hero = live.or((phase == "done").then(|| p.and_then(|p| p.download_mbps)).flatten());
        let hero_label = if running {
            match phase {
                "ping" => "Измеряем задержку…",
                "download" => "Загрузка",
                _ => "Отдача",
            }
        } else if phase == "done" {
            "Загрузка"
        } else {
            "Готов к проверке"
        };
        let round = |v: Option<f64>| v.map_or(format::DASH.to_string(), |v| v.round().to_string());
        app.set_speed_running(running);
        app.set_speed_step(step);
        app.set_speed_progress(p.map_or(0.0, |p| p.progress) as f32);
        app.set_speed_hero(sh(format::mbps(hero)));
        app.set_speed_hero_label(sh(hero_label));
        app.set_speed_down(sh(format::mbps(p.and_then(|p| p.download_mbps))));
        app.set_speed_up(sh(format::mbps(p.and_then(|p| p.upload_mbps))));
        app.set_speed_ping(sh(round(p.and_then(|p| p.ping_ms))));
        app.set_speed_jitter(sh(round(p.and_then(|p| p.jitter_ms))));
        let rows = self
            .speed_history
            .iter()
            .map(|r| SpeedRow {
                at: sh(format::date_time(Some(r.at))),
                server: sh(format::split_flag(&r.server_name).1),
                down: sh(format!("{} Мбит/с", format::mbps(Some(r.download_mbps)))),
                up: sh(format!("{} Мбит/с", format::mbps(Some(r.upload_mbps)))),
                ping: sh(format!("{} мс", r.ping_ms.round())),
            })
            .collect();
        app.set_speed_history(model(rows));
    }

    pub fn speed_started(&mut self) {
        self.speed_running = true;
        self.speed = Some(SpeedProgress {
            phase: "ping",
            mbps: 0.0,
            progress: 0.0,
            ping_ms: None,
            jitter_ms: None,
            download_mbps: None,
            upload_mbps: None,
        });
        self.push_speed();
    }

    pub fn speed_finished(&mut self, result: Result<SpeedResult, String>) {
        self.speed_running = false;
        match result {
            Ok(r) => {
                self.speed_history.insert(0, r);
                self.speed_history.truncate(10);
            }
            Err(e) if e == "отменено" => {}
            Err(e) => self.toast(ToastKind::Error, format!("Тест скорости: {e}")),
        }
        self.push_speed();
    }

    pub fn set_speed_target(&mut self, target: String) {
        self.speed_target = (target != LIVE_TARGET).then_some(target);
        self.push_speed_targets();
    }

    // Logs

    fn filtered_logs(&self) -> Vec<&LogEntry> {
        let (level, source, query) = &self.log_filter;
        let q = query.trim().to_lowercase();
        let mut list: Vec<&LogEntry> = self
            .logs
            .iter()
            .filter(|l| match level.as_str() {
                "error" => l.level == "error",
                "warning" => l.level == "error" || l.level == "warning",
                _ => true,
            })
            .filter(|l| match source.as_str() {
                "all" => true,
                "xray" => l.source.starts_with("xray"),
                other => l.source == other,
            })
            .filter(|l| q.is_empty() || l.message.to_lowercase().contains(&q))
            .collect();
        if list.len() > RENDER_LOGS {
            list.drain(..list.len() - RENDER_LOGS);
        }
        list
    }

    pub fn push_logs(&mut self) {
        if !self.logs_dirty {
            return;
        }
        self.logs_dirty = false;
        let rows: Vec<LogRow> = self
            .filtered_logs()
            .into_iter()
            .map(|l| LogRow {
                id: l.id as i32,
                time: sh(format::clock(l.ts)),
                level: match l.level {
                    "error" => 0,
                    "warning" => 1,
                    "info" => 2,
                    _ => 3,
                },
                level_text: sh(level_short(l.level)),
                source: sh(l.source),
                message: sh(&l.message),
            })
            .collect();
        let app = self.app();
        app.set_logs(model(rows));
        app.set_log_total(self.logs.len() as i32);
    }

    pub fn set_log_filter(&mut self, level: String, source: String, query: String) {
        self.log_filter = (level, source, query);
        self.logs_dirty = true;
        self.push_logs();
    }

    pub fn visible_log_text(&self) -> String {
        self.filtered_logs().into_iter().map(log_line).collect::<Vec<_>>().join("\n")
    }

    pub fn log_text(&self, id: i32) -> Option<String> {
        self.logs.iter().find(|l| l.id as i32 == id).map(log_line)
    }

    pub fn clear_logs(&mut self) {
        self.logs.clear();
        self.logs_dirty = true;
        self.push_logs();
    }

    pub fn navigate(&mut self, page: Page) {
        self.app().set_page(page);
        if page == Page::Logs {
            self.logs_dirty = true;
            self.push_logs();
        }
    }

    // Tray

    fn push_tray(&self) {
        let t = &self.tray;
        let live_name = self
            .conn
            .server_id
            .as_deref()
            .and_then(|id| self.store.servers.iter().find(|s| s.id == id))
            .map(|s| display_name(&s.name))
            .unwrap_or_default();
        t.set_app_name(sh(prism_win::profile().app_name));
        t.set_connected(self.is_connected());
        t.set_active(self.is_active());
        t.set_status_text(sh(match self.conn.status {
            ConnStatus::Connected => format!("Подключено — {live_name}"),
            ConnStatus::Connecting => "Подключение…".into(),
            ConnStatus::Disconnecting => "Отключение…".into(),
            ConnStatus::Error => "Ошибка подключения".into(),
            ConnStatus::Disconnected => "Отключено".into(),
        }));
        t.set_mode(sh(match self.settings.routing_mode {
            RoutingMode::Smart => "smart",
            RoutingMode::Global => "global",
        }));
        t.set_tun(self.settings.tun);
        let servers = self
            .store
            .servers
            .iter()
            .take(MAX_TRAY_SERVERS)
            .map(|sv| TrayServer {
                id: sh(&sv.id),
                name: sh(display_name(&sv.name)),
                selected: self.settings.selected_server.as_deref() == Some(sv.id.as_str()),
            })
            .collect();
        t.set_servers(model(servers));
    }
}

#[cfg(debug_assertions)]
pub fn dev_traffic(i: i64) -> (f64, f64) {
    let down = match i {
        17 | 18 => 1_900_000,
        31 => 2_200_000,
        44 => 2_300_000,
        52..=55 => 1_200_000,
        _ => 200_000 + ((i * 37) % 9) * 30_000,
    };
    let up = match i {
        37 => 600_000,
        49 => 450_000,
        _ => 30_000 + ((i * 13) % 7) * 12_000,
    };
    (up as f64, down as f64)
}

#[cfg(debug_assertions)]
impl View {
    pub fn dev_fake_connected(&mut self) {
        let now = format::now_millis();
        self.conn = ConnectionInfo {
            status: ConnStatus::Connected,
            server_id: self.settings.selected_server.clone().or_else(|| self.store.servers.first().map(|s| s.id.clone())),
            mode: self.settings.routing_mode,
            tun: false,
            since: Some(now - 40_000),
            ip: Some("159.195.213.223".into()),
            country: Some("de".into()),
            ..Default::default()
        };
        chart::clear();
        for i in 0..60 {
            let (up, down) = dev_traffic(i);
            chart::push(now - (59 - i) * 1000, up, down);
        }
        let (up, down) = dev_traffic(59);
        self.traffic = Some(Traffic { up, down, total_up: 3_774_873, total_down: 15_099_494 });
        self.push_conn();
        self.push_traffic();
        self.push_servers();
    }
}

pub fn chart_hover(fraction: f32) -> ChartHover {
    chart::hover(fraction)
}
