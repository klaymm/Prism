use crate::logs::LogEntry;
use crate::models::{ConnectionInfo, Latency, ServerStore, Settings};
use crate::routing::RoutingState;
use crate::service::Traffic;
use crate::speedtest::SpeedProgress;
use crate::updater::UpdateStatus;

#[derive(Clone, Debug)]
pub enum Event {
    Conn(ConnectionInfo),
    Settings(Settings),
    Store(ServerStore),
    Routing(RoutingState),
    Traffic(Traffic),
    Logs(Vec<LogEntry>),
    Latency { server_id: String, latency: Latency },
    Speed(SpeedProgress),
    Update(UpdateStatus),
}

pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: Event);
}
