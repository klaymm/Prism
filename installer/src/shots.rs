use crate::{Screen, Setup, SetupWindow, Theme};
use slint::ComponentHandle;
use std::path::PathBuf;
use std::time::Duration;

const SHOTS: &[(&str, Screen, f32, &str, bool)] = &[
    ("welcome", Screen::Welcome, 0.0, "", false),
    ("options", Screen::Options, 0.0, "", false),
    ("progress", Screen::Progress, 0.46, "extract", false),
    ("done", Screen::Done, 1.0, "register", false),
    ("error", Screen::Error, 0.0, "", false),
    ("confirm", Screen::Confirm, 0.0, "", true),
    ("uninstall-done", Screen::Done, 1.0, "", true),
];

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

pub fn start(win: SetupWindow, dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let steps: Vec<(usize, &'static str)> =
        ["light", "dark"].iter().flat_map(|t| (0..SHOTS.len()).map(move |i| (i, *t))).collect();
    step(win, dir, steps, 0);
}

fn step(win: SetupWindow, dir: PathBuf, steps: Vec<(usize, &'static str)>, i: usize) {
    let Some(&(index, theme)) = steps.get(i) else {
        let _ = slint::quit_event_loop();
        return;
    };
    let (name, screen, progress, stage, uninstalling) = SHOTS[index];
    let setup = win.global::<Setup>();
    win.global::<Theme>().set_mode(theme.into());
    setup.set_uninstalling(uninstalling);
    setup.set_progress(progress);
    setup.set_step(stage.into());
    setup.set_error("Не удалось закрыть prism.exe. Закройте программу и повторите.".into());
    setup.set_screen(screen);
    slint::Timer::single_shot(Duration::from_millis(1600), move || {
        win.window().dispatch_event(slint::platform::WindowEvent::PointerExited);
        if let Ok(buf) = win.window().take_snapshot() {
            let _ = write_bmp(&dir.join(format!("{name}-{theme}.bmp")), &buf);
        }
        step(win, dir, steps, i + 1);
    });
}
