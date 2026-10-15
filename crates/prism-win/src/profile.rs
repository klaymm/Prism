use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub app_name: &'static str,
    pub data_id: &'static str,
    pub instance_id: &'static str,
    pub dev: bool,
}

const RELEASE: Profile = Profile { app_name: "Prism", data_id: "com.klaymm.prism", instance_id: "Prism", dev: false };
const DEV: Profile =
    Profile { app_name: "Prism Dev", data_id: "com.klaymm.prism.dev", instance_id: "PrismDev", dev: true };

pub const DEV_ARG: &str = "--profile=dev";

pub fn profile() -> &'static Profile {
    static PROFILE: OnceLock<Profile> = OnceLock::new();
    PROFILE.get_or_init(|| {
        let dev = std::env::var("PRISM_PROFILE").as_deref() == Ok("dev") || std::env::args().any(|a| a == DEV_ARG);
        if dev { DEV } else { RELEASE }
    })
}

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_default()
}

impl Profile {
    pub fn args(&self) -> &'static str {
        if self.dev { DEV_ARG } else { "" }
    }

    pub fn manages_system_proxy(&self) -> bool {
        !self.dev
    }

    pub fn allows_tun(&self) -> bool {
        !self.dev
    }

    pub fn roaming_data(&self) -> PathBuf {
        env_dir("APPDATA").join(self.data_id)
    }

    pub fn local_data(&self) -> PathBuf {
        env_dir("LOCALAPPDATA").join(self.data_id)
    }
}
