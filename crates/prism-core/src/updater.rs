use crate::models::now_secs;
use crate::state::{Shared, direct_client};
use futures_util::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const LATEST_RELEASE: &str = "https://api.github.com/repos/klaymm/Prism/releases/latest";
const FEED_VAR: &str = "PRISM_UPDATE_FEED";
const SETUP_PREFIX: &str = "Prism-Setup-";
const CHECK_EVERY_SECS: i64 = 6 * 3600;
const ATTEMPT_FILE: &str = "attempt.txt";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    asset_url: String,
    asset_name: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateStatus {
    #[default]
    Unknown,
    Checking,
    Latest,
    Available(Release),
    Downloading { release: Release, progress: f32 },
    Installing(Release),
    Failed(String),
}

fn version_tuple(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect()
}

pub fn is_newer(version: &str, than: &str) -> bool {
    version_tuple(version) > version_tuple(than)
}

fn feed_url() -> String {
    match std::env::var(FEED_VAR) {
        Ok(url) if crate::win::profile().dev => url,
        _ => LATEST_RELEASE.into(),
    }
}

pub fn installed() -> bool {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("uninstall.exe").exists())).unwrap_or(false)
}

fn updates_dir() -> PathBuf {
    crate::win::profile().local_data().join("updates")
}

fn parse_release(v: &Value) -> Result<Release, String> {
    let tag = v["tag_name"].as_str().filter(|t| !t.is_empty()).ok_or("в ответе GitHub нет версии")?;
    let asset = v["assets"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|x| x["name"].as_str().is_some_and(|n| n.starts_with(SETUP_PREFIX) && n.ends_with(".exe")))
        })
        .ok_or("в релизе нет установщика")?;
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or("у установщика в релизе нет контрольной суммы")?;
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        page_url: v["html_url"].as_str().unwrap_or_default().to_string(),
        asset_url: asset["browser_download_url"].as_str().unwrap_or_default().to_string(),
        asset_name: asset["name"].as_str().unwrap_or_default().to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
        sha256: sha256.to_ascii_lowercase(),
    })
}

async fn get_json(state: &Shared, url: &str) -> Result<Value, String> {
    async fn fetch(client: &reqwest::Client, url: &str) -> Result<Value, String> {
        let resp = client
            .get(url)
            .header("User-Agent", "Prism")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("GitHub ответил {}", resp.status()));
        }
        resp.json().await.map_err(|e| e.to_string())
    }
    match fetch(&state.direct, url).await {
        Ok(v) => Ok(v),
        Err(e) => match state.tunnel_client(Duration::from_secs(30)) {
            Some(c) => fetch(&c, url).await.map_err(|_| e),
            None => Err(e),
        },
    }
}

pub async fn check(state: &Shared) -> Result<Option<Release>, String> {
    state.set_update(UpdateStatus::Checking);
    let fetched = async { parse_release(&get_json(state, &feed_url()).await?) }.await;
    let result = fetched
        .map(|r| Some(r).filter(|r| is_newer(&r.version, CURRENT)))
        .map_err(|e| format!("Не удалось проверить обновления: {e}"));
    state.last_update_check.store(now_secs(), Ordering::SeqCst);
    state.set_update(match &result {
        Ok(Some(r)) => UpdateStatus::Available(r.clone()),
        Ok(None) => UpdateStatus::Latest,
        Err(e) => UpdateStatus::Failed(e.clone()),
    });
    if let Err(e) = &result {
        state.logs.push("app", "warning", e.clone());
    }
    result
}

pub fn check_due(state: &Shared) -> bool {
    now_secs() - state.last_update_check.load(Ordering::SeqCst) >= CHECK_EVERY_SECS
}

fn clean_downloads() {
    let dir = updates_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name() != ATTEMPT_FILE {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let attempt = dir.join(ATTEMPT_FILE);
    if std::fs::read_to_string(&attempt).is_ok_and(|v| !is_newer(v.trim(), CURRENT)) {
        let _ = std::fs::remove_file(attempt);
    }
}

fn failed_before(version: &str) -> bool {
    std::fs::read_to_string(updates_dir().join(ATTEMPT_FILE)).is_ok_and(|v| v.trim() == version)
        && is_newer(version, CURRENT)
}

pub async fn check_at_launch(state: &Shared) -> Option<Release> {
    clean_downloads();
    if !installed() {
        return None;
    }
    crate::net::wait_for_network("api.github.com", crate::net::LAUNCH_WAIT).await;
    let release = check(state).await.ok().flatten()?;
    if !state.settings.read().auto_update {
        return None;
    }
    if failed_before(&release.version) {
        state.logs.push(
            "app",
            "warning",
            format!("обновление до {} уже не установилось, поставьте его вручную в настройках", release.version),
        );
        return None;
    }
    Some(release)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn fetch_file(client: &reqwest::Client, url: &str, path: &Path, progress: &mut impl FnMut(u64)) -> Result<String, String> {
    let resp = client.get(url).header("User-Agent", "Prism").send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| format!("{}: {e}", path.display()))?;
        hasher.update(&chunk);
        done += chunk.len() as u64;
        progress(done);
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(hex(&hasher.finalize()))
}

pub async fn download(state: &Shared, release: &Release) -> Result<PathBuf, String> {
    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(&release.asset_name);
    let part = dir.join(format!("{}.part", release.asset_name));
    let size = release.size.max(1) as f32;
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut report = |done: u64| {
        if last.elapsed() >= Duration::from_millis(250) {
            last = Instant::now();
            let progress = (done as f32 / size).min(1.0);
            state.set_update(UpdateStatus::Downloading { release: release.clone(), progress });
        }
    };
    report(0);
    state.logs.info(format!("загрузка Prism {}", release.version));
    let result = async {
        let sha = match fetch_file(&direct_client(Duration::from_secs(600)), &release.asset_url, &part, &mut report).await {
            Ok(sha) => sha,
            Err(e) => match state.tunnel_client(Duration::from_secs(600)) {
                Some(c) => fetch_file(&c, &release.asset_url, &part, &mut report).await.map_err(|_| e)?,
                None => return Err(e),
            },
        };
        if sha != release.sha256 {
            return Err("контрольная сумма установщика не совпала, обновление отменено".to_string());
        }
        std::fs::rename(&part, &dest).map_err(|e| format!("{}: {e}", dest.display()))
    }
    .await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        let e = format!("Не удалось загрузить обновление: {e}");
        state.set_update(UpdateStatus::Failed(e.clone()));
        state.logs.error(&e);
        return Err(e);
    }
    Ok(dest)
}

pub fn start_setup(state: &Shared, setup: &Path, release: &Release, args: Vec<String>) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().filter(|d| d.join("uninstall.exe").exists()).ok_or("Обновлять можно только установленный Prism")?;
    let opts = prism_win::InstallOptions { dir: dir.display().to_string(), launch: true, update: true, args, ..Default::default() };
    let mut cmd = std::process::Command::new(setup);
    cmd.arg(opts.to_arg());
    if crate::win::profile().dev {
        cmd.arg(crate::win::DEV_ARG);
    }
    let spawned = cmd.creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn().or_else(|_| cmd.creation_flags(0).spawn());
    if let Err(e) = spawned {
        let e = format!("Не удалось запустить установщик: {e}");
        state.set_update(UpdateStatus::Failed(e.clone()));
        return Err(e);
    }
    let _ = std::fs::write(updates_dir().join(ATTEMPT_FILE), &release.version);
    state.logs.info(format!("установка Prism {}", release.version));
    state.set_update(UpdateStatus::Installing(release.clone()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn latest(tag: &str, assets: Value) -> Value {
        json!({ "tag_name": tag, "html_url": format!("https://github.com/klaymm/Prism/releases/tag/{tag}"), "assets": assets })
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("v0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("0.4.0-beta", "0.4.0"));
    }

    #[test]
    fn picks_the_setup_with_its_checksum() {
        let v = latest(
            "v0.5.0",
            json!([
                { "name": "notes.txt", "browser_download_url": "https://x/notes.txt", "size": 3, "digest": "sha256:00" },
                { "name": "Prism-Setup-0.5.0.exe", "browser_download_url": "https://x/Prism-Setup-0.5.0.exe",
                  "size": 46090387, "digest": "sha256:82E47D3A" },
            ]),
        );
        let r = parse_release(&v).unwrap();
        assert_eq!(r.version, "0.5.0");
        assert_eq!(r.asset_name, "Prism-Setup-0.5.0.exe");
        assert_eq!(r.sha256, "82e47d3a");
        assert_eq!(r.size, 46090387);
        assert!(r.page_url.ends_with("/v0.5.0"));
    }

    #[test]
    fn refuses_releases_without_setup_or_checksum() {
        let no_digest = latest("v0.5.0", json!([{ "name": "Prism-Setup-0.5.0.exe", "browser_download_url": "https://x" }]));
        assert!(parse_release(&no_digest).unwrap_err().contains("контрольной суммы"));
        let no_setup = latest("v0.5.0", json!([{ "name": "Prism.zip", "digest": "sha256:00" }]));
        assert!(parse_release(&no_setup).unwrap_err().contains("нет установщика"));
        assert!(parse_release(&json!({ "message": "Not Found" })).is_err());
    }
}
