use crate::events::EventSink;
use crate::models::{ConnectionInfo, ServerStore, Settings};
use crate::routing::RoutingState;
use crate::state::{AppState, Shared};
use crate::storage::Paths;
use crate::{data, rt, service};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

pub struct StartOptions {
    pub data_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub app_version: String,
    pub enable_tun: bool,
    pub connect_now: bool,
}

pub fn start(events: Arc<dyn EventSink>, opts: StartOptions) -> Shared {
    let _ = rt::runtime();
    let state: Shared = Arc::new(AppState::new(events, Paths::new(opts.data_dir, opts.resource_dir)));

    service::restore_proxy(&state);
    let _ = std::fs::remove_dir_all(state.paths.data.join("cores"));
    data::init_routing(&state);
    if opts.enable_tun && state.elevated && !state.settings.read().tun {
        state.settings.write().tun = true;
        state.save_settings();
        data::apply_autostart(&state);
    }
    state.logs.info(format!(
        "{} {} запущен{}",
        crate::win::profile().app_name,
        opts.app_version,
        if state.elevated { " (администратор)" } else { "" }
    ));
    if state.settings.read().autostart {
        let st = state.clone();
        std::thread::spawn(move || data::apply_autostart(&st));
    }

    service::spawn_background_loops(state.clone());
    data::spawn_schedulers(state.clone());
    if opts.connect_now || state.settings.read().connect_on_launch {
        let st = state.clone();
        rt::spawn(async move {
            let _ = service::connect_at_launch(&st).await;
        });
    }
    state
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub settings: Settings,
    pub store: ServerStore,
    pub conn: ConnectionInfo,
    pub routing: RoutingState,
}

pub fn core_versions(state: &Shared) -> Value {
    std::fs::read(state.paths.bundled("cores/versions.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

pub fn snapshot(state: &Shared) -> Snapshot {
    Snapshot {
        settings: state.settings.read().clone(),
        store: state.store.read().clone(),
        conn: state.conn.read().clone(),
        routing: state.routing.read().clone(),
    }
}

pub fn restart_as_admin(state: &Shared) -> Result<(), String> {
    let mut args = String::from("--enable-tun");
    if state.is_connected() {
        args.push_str(" --connect");
    }
    crate::win::relaunch_elevated(&args)?;
    service::shutdown(state);
    Ok(())
}
