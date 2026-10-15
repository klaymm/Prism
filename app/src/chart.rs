use crate::ChartHover;
use prism_ui::format;
use std::cell::RefCell;
use std::collections::VecDeque;

const WINDOW_MS: i64 = 60_000;

#[derive(Clone, Copy)]
struct Point {
    t: i64,
    up: f64,
    down: f64,
}

struct History {
    points: VecDeque<Point>,
    width: f64,
    height: f64,
}

thread_local!(static HISTORY: RefCell<History> =
    const { RefCell::new(History { points: VecDeque::new(), width: 560.0, height: 116.0 }) });

pub fn resize(width: f32, height: f32) -> bool {
    HISTORY.with_borrow_mut(|h| {
        let (w, hh) = (width.max(1.0) as f64, height.max(1.0) as f64);
        let changed = (h.width - w).abs() > 0.5 || (h.height - hh).abs() > 0.5;
        h.width = w;
        h.height = hh;
        changed
    })
}

pub struct Paths {
    pub down_line: String,
    pub down_area: String,
    pub up_line: String,
    pub up_area: String,
    pub ticks: [String; 3],
    pub empty: bool,
}

pub fn push(t: i64, up: f64, down: f64) {
    HISTORY.with_borrow_mut(|h| {
        h.points.push_back(Point { t, up, down });
        while h.points.front().is_some_and(|p| t - p.t > WINDOW_MS) {
            h.points.pop_front();
        }
    });
}

pub fn clear() {
    HISTORY.with_borrow_mut(|h| h.points.clear());
}

fn max_of(points: &VecDeque<Point>) -> f64 {
    format::nice_max(points.iter().map(|p| p.up.max(p.down)).fold(0.0, f64::max))
}

pub fn paths() -> Paths {
    HISTORY.with_borrow(|h| {
        let pts = &h.points;
        let max = max_of(pts);
        let t0 = pts.back().map_or(0, |p| p.t) - WINDOW_MS;
        let (w, hh) = (h.width, h.height);
        let x = |t: i64| (t - t0) as f64 / WINDOW_MS as f64 * w;
        let y = |v: f64| hh - v.min(max) / max * hh;
        let line = |value: fn(&Point) -> f64| {
            let mut d = String::new();
            for (i, p) in pts.iter().enumerate() {
                d.push_str(&format!("{}{:.1} {:.1} ", if i == 0 { "M" } else { "L" }, x(p.t), y(value(p))));
            }
            d
        };
        let area = |line: &str| match (pts.front(), pts.back()) {
            (Some(first), Some(last)) if pts.len() > 1 => {
                format!("{line}L{:.1} {hh:.1} L{:.1} {hh:.1} Z", x(last.t), x(first.t))
            }
            _ => String::new(),
        };
        let (down_line, up_line) = (line(|p| p.down), line(|p| p.up));
        Paths {
            down_area: area(&down_line),
            up_area: area(&up_line),
            down_line,
            up_line,
            ticks: ["0".into(), format::rate(max / 2.0), format::rate(max)],
            empty: pts.is_empty(),
        }
    })
}

pub fn hover(fraction: f32) -> ChartHover {
    HISTORY.with_borrow(|h| {
        let pts = &h.points;
        if fraction < 0.0 || pts.len() < 2 {
            return ChartHover::default();
        }
        let now = pts.back().unwrap().t;
        let t0 = now - WINDOW_MS;
        let t = t0 + (fraction as f64 * WINDOW_MS as f64) as i64;
        let p = *pts.iter().min_by_key(|p| (p.t - t).abs()).unwrap();
        let max = max_of(pts);
        let secs = ((now - p.t) as f64 / 1000.0).round() as i64;
        ChartHover {
            visible: true,
            x: ((p.t - t0) as f64 / WINDOW_MS as f64) as f32,
            down_y: (1.0 - p.down.min(max) / max) as f32,
            up_y: (1.0 - p.up.min(max) / max) as f32,
            ago: if secs == 0 { "сейчас".into() } else { format!("{secs} с назад").into() },
            down: format::rate(p.down).into(),
            up: format::rate(p.up).into(),
        }
    })
}
