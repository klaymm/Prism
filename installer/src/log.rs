use std::io::Write;
use std::path::PathBuf;

pub fn path() -> PathBuf {
    std::env::temp_dir().join("prism-setup.log")
}

pub fn write(msg: impl AsRef<str>) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path()) {
        let _ = writeln!(f, "[{secs}] {}", msg.as_ref());
    }
}
