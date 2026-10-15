use serde_json::{Value, json};

pub const INTERFACE_NAME: &str = "Prism";

pub struct TunInput<'a> {
    pub socks_port: u16,
    pub server_ip: Option<&'a str>,
    pub stack: &'a str,
    pub mtu: u32,
    pub bypass_processes: &'a [String],
    pub direct_apps: &'a [String],
}

pub fn build(input: &TunInput) -> Value {
    let mut rules = vec![json!({ "process_name": input.bypass_processes, "outbound": "direct" })];
    if !input.direct_apps.is_empty() {
        let patterns: Vec<String> = input.direct_apps.iter().map(|a| crate::rules::app_regex(a)).collect();
        rules.push(json!({ "process_path_regex": patterns, "outbound": "direct" }));
    }
    if let Some(ip) = input.server_ip {
        let cidr = if ip.contains(':') { format!("{ip}/128") } else { format!("{ip}/32") };
        rules.push(json!({ "ip_cidr": [cidr], "outbound": "direct" }));
    }
    rules.push(json!({ "port": [53], "outbound": "xray" }));
    rules.push(json!({ "ip_is_private": true, "outbound": "direct" }));

    json!({
        "log": { "level": "info", "timestamp": false },
        "inbounds": [{
            "type": "tun",
            "tag": "tun-in",
            "interface_name": INTERFACE_NAME,
            "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
            "mtu": input.mtu,
            "auto_route": true,
            "strict_route": true,
            "stack": input.stack,
        }],
        "outbounds": [
            { "type": "socks", "tag": "xray", "server": "127.0.0.1", "server_port": input.socks_port, "version": "5" },
            { "type": "direct", "tag": "direct" },
        ],
        "route": {
            "auto_detect_interface": true,
            "rules": rules,
            "final": "xray",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_with(apps: &[String]) -> Value {
        let procs = vec!["xray.exe".to_string(), "sing-box.exe".to_string(), "prism.exe".to_string()];
        build(&TunInput {
            socks_port: 10808,
            server_ip: Some("203.0.113.7"),
            stack: "mixed",
            mtu: 9000,
            bypass_processes: &procs,
            direct_apps: apps,
        })
    }

    fn sample() -> Value {
        sample_with(&[])
    }

    #[test]
    fn user_programs_bypass_before_dns() {
        let apps = vec!["Doomsday.exe".to_string()];
        let cfg = sample_with(&apps);
        let rules = cfg["route"]["rules"].as_array().unwrap();
        assert_eq!(rules[1], json!({ "process_path_regex": [r"(?i)(^|\\)Doomsday\.exe$"], "outbound": "direct" }));
        let dns = rules.iter().position(|r| r.get("port").is_some()).unwrap();
        assert!(dns > 1, "a program's own DNS queries skip the VPN too");
        assert!(sample()["route"]["rules"].as_array().unwrap().iter().all(|r| r.get("process_path_regex").is_none()));
    }

    #[test]
    fn dns_rule_precedes_private_bypass() {
        let cfg = sample();
        let rules = cfg["route"]["rules"].as_array().unwrap();
        let dns = rules.iter().position(|r| r.get("port").is_some()).unwrap();
        let private = rules.iter().position(|r| r.get("ip_is_private").is_some()).unwrap();
        assert!(dns < private);
        assert_eq!(rules[1]["ip_cidr"][0], "203.0.113.7/32");
        assert_eq!(cfg["route"]["final"], "xray");
    }

    #[test]
    fn dump_config_for_core_validation() {
        let dir = crate::test_output_dir("prism-config-check");
        std::fs::write(dir.join("singbox-tun.json"), serde_json::to_string_pretty(&sample()).unwrap()).unwrap();
        let apps = vec!["Doomsday.exe".to_string(), "Last Day (x64).exe".to_string()];
        std::fs::write(dir.join("singbox-tun-apps.json"), serde_json::to_string_pretty(&sample_with(&apps)).unwrap()).unwrap();
    }
}
