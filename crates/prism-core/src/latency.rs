use crate::core::{CoreProcess, free_port, wait_for_port};
use crate::models::*;
use crate::state::{Shared, socks_client};
use crate::storage::write_atomic;
use crate::xray_config;
use futures_util::StreamExt;
use futures_util::stream;
use crate::events::Event;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub const PROBE_URL: &str = "https://www.gstatic.com/generate_204";
const CONCURRENCY: usize = 8;
const BATCH: usize = 64;

pub async fn tcp_ping(host: &str, port: u16) -> i64 {
    let Ok(Ok(mut addrs)) =
        tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host((host, port))).await
    else {
        return -1;
    };
    let Some(addr) = addrs.next() else { return -1 };
    let start = Instant::now();
    match tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_)) => start.elapsed().as_millis().max(1) as i64,
        _ => -1,
    }
}

pub async fn http_delay(client: &reqwest::Client) -> i64 {
    let mut best = -1i64;
    for _ in 0..2 {
        let start = Instant::now();
        if let Ok(resp) = client.get(PROBE_URL).send().await {
            if resp.status().as_u16() < 400 {
                let ms = start.elapsed().as_millis().max(1) as i64;
                best = if best < 0 { ms } else { best.min(ms) };
            }
        }
    }
    best
}

pub async fn start_probe(state: &Shared, servers: &[Server]) -> Result<(CoreProcess, Vec<u16>), String> {
    let ports: Vec<u16> = servers.iter().map(|_| free_port()).collect();
    let pairs: Vec<(u16, &VlessConfig)> = ports.iter().copied().zip(servers.iter().map(|s| &s.config)).collect();
    let cfg = xray_config::build_probe(&pairs);
    let path = state.paths.run.join(format!("probe-{}.json", uuid::Uuid::new_v4().simple()));
    write_atomic(&path, &serde_json::to_vec(&cfg).unwrap_or_default())?;
    let since = state.logs.last_id();
    let arg = path.to_string_lossy().to_string();
    let spawned = CoreProcess::spawn(
        "xray-probe",
        &state.paths.core_exe("xray.exe"),
        &["run", "-c", &arg],
        &[("XRAY_LOCATION_ASSET", &state.paths.geo)],
        &state.paths.run,
        &state.logs,
        state.job.as_ref(),
    );
    let result = match spawned {
        Ok(mut proc) => match wait_for_port(&mut proc, ports[0], Duration::from_secs(8), &state.logs, since).await {
            Ok(()) => Ok((proc, ports)),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&path);
    result
}

fn record(state: &Shared, server_id: &str, latency: Latency) {
    if let Some(s) = state.store.write().servers.iter_mut().find(|s| s.id == server_id) {
        s.latency = Some(latency.clone());
    }
    state.emit(Event::Latency { server_id: server_id.to_string(), latency });
}

pub async fn ping(state: &Shared, ids: Option<Vec<String>>, kind: LatencyKind) -> Result<(), String> {
    if state.ping_running.swap(true, Ordering::SeqCst) {
        return Err("Проверка уже идёт".into());
    }
    let servers: Vec<Server> = {
        let store = state.store.read();
        store
            .servers
            .iter()
            .filter(|s| ids.as_ref().is_none_or(|ids| ids.contains(&s.id)))
            .cloned()
            .collect()
    };
    let result = match kind {
        LatencyKind::Tcp => {
            stream::iter(servers)
                .for_each_concurrent(16, |s| async move {
                    let ms = tcp_ping(&s.config.address, s.config.port).await;
                    record(state, &s.id, Latency { ms, kind, at: now_secs() });
                })
                .await;
            Ok(())
        }
        LatencyKind::Real => real_delay(state, servers).await,
    };
    state.ping_running.store(false, Ordering::SeqCst);
    state.save_store();
    result
}

async fn real_delay(state: &Shared, servers: Vec<Server>) -> Result<(), String> {
    for chunk in servers.chunks(BATCH) {
        let (mut proc, ports) = start_probe(state, chunk).await?;
        stream::iter(chunk.iter().zip(ports))
            .for_each_concurrent(CONCURRENCY, |(s, port)| async move {
                let ms = match socks_client(port, Duration::from_secs(6)) {
                    Ok(client) => http_delay(&client).await,
                    Err(_) => -1,
                };
                record(state, &s.id, Latency { ms, kind: LatencyKind::Real, at: now_secs() });
            })
            .await;
        proc.kill();
    }
    Ok(())
}
