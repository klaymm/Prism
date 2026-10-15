pub mod api;
pub mod core;
pub mod data;
#[cfg(test)]
mod e2e;
pub mod events;
pub mod latency;
pub mod logs;
pub mod models;
pub mod net;
pub mod routing;
pub mod rt;
pub mod rules;
pub mod service;
pub mod share_link;
pub mod singbox_config;
pub mod speedtest;
pub mod state;
pub mod storage;
pub mod subscription;
pub mod updater;
pub mod xray_config;

pub use prism_win as win;

#[cfg(test)]
fn test_output_dir(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
