use serde::{Deserialize, Serialize};

pub const AUTO_INSTALL_ARG: &str = "--auto-install";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    pub dir: String,
    pub desktop: bool,
    pub autostart: bool,
    pub launch: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub args: Vec<String>,
}

impl InstallOptions {
    pub fn to_arg(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let hex: String = json.bytes().map(|b| format!("{b:02x}")).collect();
        format!("{AUTO_INSTALL_ARG}={hex}")
    }

    pub fn from_arg(hex: &str) -> Option<Self> {
        let bytes: Option<Vec<u8>> =
            (0..hex.len()).step_by(2).map(|i| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect();
        serde_json::from_slice(&bytes?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_survive_the_command_line() {
        let opts = InstallOptions {
            dir: r"C:\Users\Аня\AppData\Local\Programs\Prism".into(),
            launch: true,
            update: true,
            args: vec!["--autostart".into(), "--connect".into()],
            ..Default::default()
        };
        let arg = opts.to_arg();
        assert!(arg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'='));
        let value = arg.strip_prefix(&format!("{AUTO_INSTALL_ARG}=")).unwrap();
        assert_eq!(InstallOptions::from_arg(value), Some(opts));
        assert_eq!(InstallOptions::from_arg("zz"), None);
    }

    #[test]
    fn options_from_older_prism_still_read() {
        let json = r#"{"dir":"C:\\P\\Prism","desktop":true,"autostart":false,"launch":true}"#;
        let hex: String = json.bytes().map(|b| format!("{b:02x}")).collect();
        let opts = InstallOptions::from_arg(&hex).unwrap();
        assert!(opts.desktop && opts.launch && !opts.update && opts.args.is_empty());
    }
}
