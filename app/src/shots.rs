use crate::{App, AppWindow, Page, Theme};
use slint::ComponentHandle;
use std::path::PathBuf;
use std::time::Duration;

struct Shot {
    name: &'static str,
    page: Page,
    overlay: &'static str,
}

const SHOTS: &[Shot] = &[
    Shot { name: "home", page: Page::Home, overlay: "" },
    Shot { name: "servers", page: Page::Servers, overlay: "" },
    Shot { name: "routing", page: Page::Routing, overlay: "" },
    Shot { name: "speed", page: Page::Speed, overlay: "" },
    Shot { name: "logs", page: Page::Logs, overlay: "" },
    Shot { name: "settings", page: Page::Settings, overlay: "" },
    Shot { name: "import", page: Page::Home, overlay: "import" },
    Shot { name: "elevation", page: Page::Home, overlay: "elevation" },
    Shot { name: "hover", page: Page::Home, overlay: "hover" },
];

const HOVER_POINT: (f32, f32) = (600.0, 470.0);

fn write_bmp(path: &PathBuf, buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) -> std::io::Result<()> {
    let (w, h) = (buf.width(), buf.height());
    let row = w * 4;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for px in buf.as_slice() {
        out.extend_from_slice(&[px.b, px.g, px.r, 255]);
    }
    std::fs::write(path, out)
}

pub fn start(win: AppWindow, dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let only = std::env::var("PRISM_DEV_SHOTS_ONLY").unwrap_or_default();
    let wanted = |s: &Shot| only.is_empty() || only.split(',').any(|n| n == s.name);
    let steps: Vec<(&'static Shot, &'static str)> = ["light", "dark"]
        .iter()
        .flat_map(|theme| SHOTS.iter().filter(|s| wanted(s)).map(move |s| (s, *theme)))
        .collect();
    step(win, dir, steps, 0);
}

fn step(win: AppWindow, dir: PathBuf, steps: Vec<(&'static Shot, &'static str)>, i: usize) {
    let Some(&(shot, theme)) = steps.get(i) else {
        let _ = slint::quit_event_loop();
        return;
    };
    let app = win.global::<App>();
    win.global::<Theme>().set_mode(theme.into());
    app.set_theme(theme.into());
    app.set_import_open(shot.overlay == "import");
    app.set_elevation_open(shot.overlay == "elevation");
    crate::view::with(|v| v.navigate(shot.page));
    slint::Timer::single_shot(Duration::from_millis(700), move || {
        let pointer = if shot.overlay == "hover" {
            slint::platform::WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(HOVER_POINT.0, HOVER_POINT.1),
            }
        } else {
            slint::platform::WindowEvent::PointerExited
        };
        win.window().dispatch_event(pointer);
        win.global::<crate::TipState>().set_visible(false);
        if let Ok(buf) = win.window().take_snapshot() {
            let _ = write_bmp(&dir.join(format!("{}-{theme}.bmp", shot.name)), &buf);
        }
        step(win, dir, steps, i + 1);
    });
}
