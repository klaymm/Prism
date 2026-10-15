use crate::events::Event;
use crate::latency::start_probe;
use crate::models::*;
use crate::state::{SPEED_FILE, Shared, socks_client};
use crate::storage::save_json;
use bytes::Bytes;
use futures_util::{StreamExt, future::join_all, stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const PING_URL: &str = "https://speed.cloudflare.com/__down?bytes=0";
const DOWN_URL: &str = "https://speed.cloudflare.com/__down?bytes=25000000";
const UP_URL: &str = "https://speed.cloudflare.com/__up";
const UP_CHUNK: usize = 64 * 1024;
const UP_REQUEST: usize = 8 * 1024 * 1024;
const WARMUP: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default)]
pub struct SpeedProgress {
    pub phase: &'static str,
    pub mbps: f64,
    pub progress: f64,
    pub ping_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub download_mbps: Option<f64>,
    pub upload_mbps: Option<f64>,
}

fn cancelled(state: &Shared) -> bool {
    state.speedtest_cancel.load(Ordering::SeqCst)
}

fn to_mbps(bytes: u64, secs: f64) -> f64 {
    if secs <= 0.0 { 0.0 } else { bytes as f64 * 8.0 / secs / 1_000_000.0 }
}

pub async fn run(state: &Shared, server_id: Option<String>) -> Result<SpeedResult, String> {
    if state.speedtest_running.swap(true, Ordering::SeqCst) {
        return Err("Тест скорости уже идёт".into());
    }
    state.speedtest_cancel.store(false, Ordering::SeqCst);
    let result = run_inner(state, server_id).await;
    state.speedtest_running.store(false, Ordering::SeqCst);
    if let Err(e) = &result {
        state.emit(Event::Speed(SpeedProgress { phase: "error", ..Default::default() }));
        state.logs.push("app", "warning", format!("тест скорости: {e}"));
    }
    result
}

async fn run_inner(state: &Shared, server_id: Option<String>) -> Result<SpeedResult, String> {
    let live = if state.is_connected() { state.conn.read().server_id.clone() } else { None };
    let use_live = live.is_some() && (server_id.is_none() || server_id == live);
    let target_id = if use_live { live.clone() } else { server_id.or_else(|| state.current_server().map(|s| s.id)) };
    let server = target_id.as_deref().and_then(|id| state.find_server(id)).ok_or("Нет сервера для теста")?;

    let mut probe = None;
    let port = if use_live {
        state.settings.read().socks_port
    } else {
        let (proc, ports) = start_probe(state, std::slice::from_ref(&server)).await?;
        probe = Some(proc);
        ports[0]
    };
    let client = socks_client(port, Duration::from_secs(30))?;
    let result = measure_all(state, &client, &server.name).await;
    if let Some(mut p) = probe {
        p.kill();
    }
    let result = result?;

    {
        let mut history = state.speed_history.write();
        history.insert(0, result.clone());
        history.truncate(10);
        let _ = save_json(&state.paths.file(SPEED_FILE), &*history);
    }
    Ok(result)
}

async fn measure_all(state: &Shared, client: &reqwest::Client, server_name: &str) -> Result<SpeedResult, String> {
    let emit = |p: SpeedProgress| state.emit(Event::Speed(p));

    emit(SpeedProgress { phase: "ping", ..Default::default() });
    let mut samples = Vec::new();
    for i in 0..10 {
        if cancelled(state) {
            return Err("отменено".into());
        }
        let start = Instant::now();
        if let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(5), client.get(PING_URL).send()).await {
            if resp.status().is_success() {
                let _ = resp.bytes().await;
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        emit(SpeedProgress { phase: "ping", progress: (i + 1) as f64 / 10.0, ..Default::default() });
    }
    if samples.is_empty() {
        return Err("сервер теста скорости недоступен через туннель".into());
    }
    if samples.len() > 2 {
        samples.remove(0);
    }
    let jitter = if samples.len() > 1 {
        samples.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (samples.len() - 1) as f64
    } else {
        0.0
    };
    let mut sorted = samples.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let ping = sorted[sorted.len() / 2];
    let base = SpeedProgress { ping_ms: Some(ping), jitter_ms: Some(jitter), ..Default::default() };

    let download = measure(state, client, "download", Duration::from_secs(10), 4, &base).await;
    if cancelled(state) {
        return Err("отменено".into());
    }
    let with_down = SpeedProgress { download_mbps: Some(download), ..base.clone() };
    let upload = measure(state, client, "upload", Duration::from_secs(8), 3, &with_down).await;
    if cancelled(state) {
        return Err("отменено".into());
    }

    let result = SpeedResult {
        server_name: server_name.to_string(),
        download_mbps: download,
        upload_mbps: upload,
        ping_ms: ping,
        jitter_ms: jitter,
        at: now_secs(),
    };
    emit(SpeedProgress { phase: "done", progress: 1.0, upload_mbps: Some(upload), ..with_down });
    Ok(result)
}

async fn download_worker(client: reqwest::Client, counter: Arc<AtomicU64>, state: Shared) {
    while !cancelled(&state) {
        let Ok(resp) = client.get(DOWN_URL).send().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(c) => {
                    counter.fetch_add(c.len() as u64, Ordering::Relaxed);
                }
                Err(_) => break,
            }
            if cancelled(&state) {
                return;
            }
        }
    }
}

async fn upload_worker(client: reqwest::Client, counter: Arc<AtomicU64>, state: Shared) {
    let chunk = Bytes::from((0..UP_CHUNK).map(|i| (i * 31 % 251) as u8).collect::<Vec<u8>>());
    while !cancelled(&state) {
        let (chunk, counter) = (chunk.clone(), counter.clone());
        let body = stream::unfold(0usize, move |sent| {
            let (chunk, counter) = (chunk.clone(), counter.clone());
            async move {
                if sent >= UP_REQUEST {
                    return None;
                }
                counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                Some((Ok::<Bytes, std::io::Error>(chunk), sent + UP_CHUNK))
            }
        });
        if client.post(UP_URL).body(reqwest::Body::wrap_stream(body)).send().await.is_err() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

async fn measure(
    state: &Shared,
    client: &reqwest::Client,
    phase: &'static str,
    duration: Duration,
    workers: usize,
    base: &SpeedProgress,
) -> f64 {
    let counter = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + duration;

    let tasks = (0..workers).map(|_| {
        let (client, counter, st) = (client.clone(), counter.clone(), state.clone());
        async move {
            let work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = if phase == "download" {
                Box::pin(download_worker(client, counter, st))
            } else {
                Box::pin(upload_worker(client, counter, st))
            };
            let _ = tokio::time::timeout_at(deadline, work).await;
        }
    });

    let reporter = async {
        let mut window: Vec<(Instant, u64)> = vec![(start, 0)];
        let mut warm_bytes = None;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let now = Instant::now();
            let bytes = counter.load(Ordering::Relaxed);
            if warm_bytes.is_none() && now.duration_since(start) >= WARMUP {
                warm_bytes = Some((now, bytes));
            }
            window.push((now, bytes));
            window.retain(|(t, _)| now.duration_since(*t) <= Duration::from_millis(1500));
            let (t0, b0) = window[0];
            let current = to_mbps(bytes - b0, now.duration_since(t0).as_secs_f64());
            let progress = (now.duration_since(start).as_secs_f64() / duration.as_secs_f64()).min(1.0);
            state.emit(Event::Speed(SpeedProgress { phase, mbps: current, progress, ..base.clone() }));
            if tokio::time::Instant::now() >= deadline || cancelled(state) {
                let (wt, wb) = warm_bytes.unwrap_or((start, 0));
                return to_mbps(bytes - wb, now.duration_since(wt).as_secs_f64());
            }
        }
    };

    let (_, mbps) = tokio::join!(join_all(tasks), reporter);
    mbps
}
