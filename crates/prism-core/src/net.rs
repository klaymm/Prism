use crate::core::CANCELLED;
use std::future::Future;
use std::time::{Duration, Instant};

pub const LAUNCH_WAIT: Duration = Duration::from_secs(300);
const RETRY_EVERY: Duration = Duration::from_secs(3);

pub async fn resolve_ipv4(host: &str, port: u16) -> Result<String, String> {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(host.to_string());
    }
    let addrs: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("не удалось определить адрес сервера {host}: {e}"))?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .map(|a| a.ip().to_string())
        .ok_or_else(|| format!("не удалось определить адрес сервера {host}"))
}

pub async fn retry<T, Fut>(
    limit: Duration,
    every: Duration,
    stop: impl Fn() -> bool,
    on_wait: impl FnOnce(),
    mut attempt: impl FnMut() -> Fut,
) -> Result<T, String>
where
    Fut: Future<Output = Result<T, String>>,
{
    let deadline = Instant::now() + limit;
    let mut on_wait = Some(on_wait);
    loop {
        match attempt().await {
            Ok(v) => return Ok(v),
            Err(e) if Instant::now() + every > deadline => return Err(e),
            Err(_) => {
                if let Some(f) = on_wait.take() {
                    f();
                }
                tokio::time::sleep(every).await;
                if stop() {
                    return Err(CANCELLED.into());
                }
            }
        }
    }
}

pub async fn resolve_patiently(
    host: &str,
    port: u16,
    limit: Duration,
    stop: impl Fn() -> bool,
    on_wait: impl FnOnce(),
) -> Result<String, String> {
    retry(limit, RETRY_EVERY, stop, on_wait, || resolve_ipv4(host, port)).await
}

pub async fn wait_for_network(host: &str, limit: Duration) {
    if !host.is_empty() {
        let _ = resolve_patiently(host, 443, limit, || false, || {}).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const TICK: Duration = Duration::from_millis(5);

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Cell::new(0);
        let waits = Cell::new(0);
        let r = retry(Duration::from_secs(5), TICK, || false, || waits.set(waits.get() + 1), || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move { if n < 3 { Err("нет сети".to_string()) } else { Ok(n) } }
        })
        .await;
        assert_eq!(r, Ok(3));
        assert_eq!(waits.get(), 1, "the wait is announced once");
    }

    #[tokio::test]
    async fn without_a_limit_fails_at_once() {
        let calls = Cell::new(0);
        let r: Result<(), String> = retry(Duration::ZERO, TICK, || false, || {}, || {
            calls.set(calls.get() + 1);
            async { Err("нет сети".to_string()) }
        })
        .await;
        assert_eq!(r, Err("нет сети".to_string()));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn stop_cancels_the_wait() {
        let r: Result<(), String> =
            retry(Duration::from_secs(5), TICK, || true, || {}, || async { Err("нет сети".to_string()) }).await;
        assert_eq!(r, Err(CANCELLED.to_string()));
    }

    #[tokio::test]
    async fn gives_up_after_the_limit() {
        let r: Result<(), String> =
            retry(Duration::from_millis(30), TICK, || false, || {}, || async { Err("нет сети".to_string()) }).await;
        assert_eq!(r, Err("нет сети".to_string()));
    }
}
