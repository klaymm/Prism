use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub geo: PathBuf,
    pub run: PathBuf,
    pub resources: PathBuf,
}

impl Paths {
    pub fn new(data: PathBuf, resources: PathBuf) -> Self {
        let p = Self {
            geo: data.join("geo"),
            run: data.join("run"),
            data,
            resources,
        };
        for dir in [&p.data, &p.geo, &p.run] {
            let _ = std::fs::create_dir_all(dir);
        }
        p
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.data.join(name)
    }

    pub fn core_exe(&self, exe: &str) -> PathBuf {
        self.resources.join("cores").join(exe)
    }

    pub fn bundled(&self, rel: &str) -> PathBuf {
        self.resources.join(rel)
    }
}

pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}
