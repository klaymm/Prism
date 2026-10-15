use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"PRISMPK1";
const TRAILER_LEN: u64 = 24;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub offset: u64,
    pub len: u64,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub exe: String,
    pub base: u64,
    pub files: Vec<Entry>,
}

impl Manifest {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

pub struct Payload {
    pub source: PathBuf,
    pub manifest: Manifest,
}

pub fn locate() -> Option<Payload> {
    let source = std::env::var_os("PRISM_PAYLOAD").map(PathBuf::from).or_else(|| std::env::current_exe().ok())?;
    let manifest = read_manifest(&source).ok()?;
    Some(Payload { source, manifest })
}

pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    if size < TRAILER_LEN {
        return Err("нет данных установки".into());
    }
    f.seek(SeekFrom::Start(size - TRAILER_LEN)).map_err(|e| e.to_string())?;
    let mut trailer = [0u8; TRAILER_LEN as usize];
    f.read_exact(&mut trailer).map_err(|e| e.to_string())?;
    if &trailer[..8] != MAGIC {
        return Err("нет данных установки".into());
    }
    let offset = u64::from_le_bytes(trailer[8..16].try_into().unwrap());
    let len = u64::from_le_bytes(trailer[16..24].try_into().unwrap());
    if offset + len + TRAILER_LEN > size {
        return Err("повреждённые данные установки".into());
    }
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_slice(&buf).map_err(|e| format!("повреждённый манифест: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = Path::new(rel);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("недопустимый путь в пакете: {}", rel.display()));
    }
    Ok(root.join(rel))
}

pub fn extract(payload: &Payload, dest: &Path, mut progress: impl FnMut(u64)) -> Result<(), String> {
    let mut src = File::open(&payload.source).map_err(|e| e.to_string())?;
    let mut done = 0u64;
    for entry in &payload.manifest.files {
        let target = safe_join(dest, &entry.path)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        src.seek(SeekFrom::Start(entry.offset)).map_err(|e| e.to_string())?;
        let compressed = (&mut src).take(entry.len);
        let mut reader = brotli_decompressor::Decompressor::new(compressed, 1 << 16);
        let tmp = target.with_extension("partial");
        let mut out = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut written = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("{}: {e}", entry.path))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", tmp.display()))?;
            hasher.update(&buf[..n]);
            written += n as u64;
            done += n as u64;
            progress(done);
        }
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        if written != entry.size || hex(&hasher.finalize()) != entry.sha256 {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{}: файл повреждён — скачайте установщик заново", entry.path));
        }
        std::fs::rename(&tmp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(())
}

pub fn write_bare_exe(payload: &Payload, dest: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let len = if payload.source == exe { payload.manifest.base } else { u64::MAX };
    let mut src = File::open(&exe).map_err(|e| e.to_string())?;
    let mut out = File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    std::io::copy(&mut (&mut src).take(len), &mut out).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(dir: &Path, prefix: &[u8], files: &[(&str, Vec<u8>)]) -> PathBuf {
        let path = dir.join("setup.exe");
        let mut out = prefix.to_vec();
        let base = out.len() as u64;
        let mut entries = Vec::new();
        for (name, data) in files {
            let mut compressed = Vec::new();
            {
                let mut w = brotli::CompressorWriter::new(&mut compressed, 4096, 9, 22);
                w.write_all(data).unwrap();
            }
            entries.push(Entry {
                path: name.to_string(),
                offset: out.len() as u64,
                len: compressed.len() as u64,
                size: data.len() as u64,
                sha256: hex(&Sha256::digest(data)),
            });
            out.extend_from_slice(&compressed);
        }
        let manifest = Manifest { name: "Prism".into(), version: "0.2.0".into(), exe: "prism.exe".into(), base, files: entries };
        let json = serde_json::to_vec(&manifest).unwrap();
        let offset = out.len() as u64;
        out.extend_from_slice(&json);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(json.len() as u64).to_le_bytes());
        std::fs::write(&path, out).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("prism-setup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_extracts_and_verifies() {
        let dir = temp_dir("roundtrip");
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let src = pack(&dir, b"MZ-fake-exe", &[("prism.exe", big.clone()), ("cores/versions.json", b"{}".to_vec())]);
        let payload = Payload { source: src.clone(), manifest: read_manifest(&src).unwrap() };
        assert_eq!(payload.manifest.base, 11);
        assert_eq!(payload.manifest.total_size(), 300_002);

        let dest = dir.join("out");
        let mut last = 0;
        extract(&payload, &dest, |n| last = n).unwrap();
        assert_eq!(last, 300_002);
        assert_eq!(std::fs::read(dest.join("prism.exe")).unwrap(), big);
        assert_eq!(std::fs::read(dest.join("cores/versions.json")).unwrap(), b"{}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_corruption_and_missing_payload() {
        let dir = temp_dir("corrupt");
        let src = pack(&dir, b"MZ", &[("a.bin", vec![1, 2, 3, 4, 5])]);
        let mut manifest = read_manifest(&src).unwrap();
        manifest.files[0].sha256 = "00".repeat(32);
        let err = extract(&Payload { source: src, manifest }, &dir.join("out"), |_| {}).unwrap_err();
        assert!(err.contains("повреждён"));

        let plain = dir.join("plain.exe");
        std::fs::write(&plain, b"MZ just an exe without payload").unwrap();
        assert!(read_manifest(&plain).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_path_traversal() {
        let root = Path::new("C:\\install");
        assert!(safe_join(root, "..\\evil.exe").is_err());
        assert!(safe_join(root, "C:\\Windows\\x.exe").is_err());
        assert!(safe_join(root, "cores/xray.exe").is_ok());
    }
}
