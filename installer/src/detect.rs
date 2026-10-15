use std::path::PathBuf;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

pub fn app_name() -> &'static str {
    prism_win::profile().app_name
}

#[derive(Clone, Debug)]
pub struct Existing {
    pub version: String,
    pub location: PathBuf,
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

fn is_prism(display_name: &str, publisher: &str, uninstall: &str, app: &str) -> bool {
    display_name.eq_ignore_ascii_case(app) && (publisher.eq_ignore_ascii_case("klaymm") || uninstall.contains("--uninstall"))
}

pub fn find() -> Vec<Existing> {
    let Ok(root) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(UNINSTALL_KEY, KEY_READ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key_name in root.enum_keys().flatten() {
        let Ok(key) = root.open_subkey_with_flags(&key_name, KEY_READ) else { continue };
        let get = |n: &str| key.get_value::<String, _>(n).unwrap_or_default();
        if !is_prism(&get("DisplayName"), &get("Publisher"), &get("UninstallString"), app_name()) {
            continue;
        }
        let location = unquote(&get("InstallLocation"));
        if location.is_empty() {
            continue;
        }
        out.push(Existing { version: get("DisplayVersion"), location: PathBuf::from(location) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_own_entries() {
        assert!(is_prism("Prism", "klaymm", r#""C:\x\uninstall.exe" --uninstall"#, "Prism"));
        assert!(!is_prism("Prism", "Someone Else", r#""C:\x\unins000.exe""#, "Prism"));
        assert!(!is_prism("Discord", "Discord Inc.", "", "Prism"));
        assert!(!is_prism("Prism", "klaymm", "--uninstall", "Prism Dev"));
        assert!(is_prism("Prism Dev", "klaymm", "--uninstall", "Prism Dev"));
    }
}
