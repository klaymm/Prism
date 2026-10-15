use crate::core::free_port;
use crate::models::RoutingMode;
use crate::routing::parse_deeplink;
use crate::share_link::parse_vless;
use crate::xray_config::{MainInput, build_main};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

fn spawn(config: &Value, name: &str) -> Proc {
    let dir = crate::test_output_dir("prism-e2e");
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(config).unwrap()).unwrap();
    let child = Command::new(resources().join("cores/xray.exe"))
        .args(["run", "-c", path.to_str().unwrap()])
        .env("XRAY_LOCATION_ASSET", resources().join("geo"))
        .stdout(Stdio::from(std::fs::File::create(dir.join(format!("{name}.log"))).unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Proc(child)
}

fn wait_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "port {port} did not open");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn x25519() -> (String, String) {
    let out = Command::new(resources().join("cores/xray.exe")).arg("x25519").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let field = |names: &[&str]| {
        text.lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                names.iter().any(|n| k.trim().to_ascii_lowercase().starts_with(&n.to_ascii_lowercase())).then(|| v.trim().to_string())
            })
            .unwrap_or_else(|| panic!("no {names:?} in x25519 output: {text}"))
    };
    (field(&["PrivateKey", "Private key"]), field(&["Password", "Public key", "PublicKey"]))
}

async fn counters(client: &reqwest::Client, metrics: u16) -> (u64, u64) {
    let v: Value = client
        .get(format!("http://127.0.0.1:{metrics}/debug/vars"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let n = |tag: &str| v["stats"]["outbound"][tag]["downlink"].as_u64().unwrap_or(0);
    (n("proxy"), n("direct"))
}

#[tokio::test]
#[ignore = "запускает настоящие ядра Xray и ходит в интернет"]
async fn reality_tunnel_and_routing() {
    let (private_key, public_key) = x25519();
    let uuid = uuid::Uuid::new_v4().to_string();
    let server_port = free_port();
    let server = json!({
        "log": { "loglevel": "warning" },
        "inbounds": [{
            "listen": "127.0.0.1", "port": server_port, "protocol": "vless",
            "settings": { "clients": [{ "id": uuid, "flow": "xtls-rprx-vision" }], "decryption": "none" },
            "streamSettings": { "network": "tcp", "security": "reality", "realitySettings": {
                "dest": "www.google.com:443", "serverNames": ["www.google.com"],
                "privateKey": private_key, "shortIds": ["", "6ba85179e30d4fc2"] } },
        }],
        "outbounds": [{ "protocol": "freedom" }],
    });
    let _server = spawn(&server, "server");
    wait_port(server_port);

    let link = format!(
        "vless://{uuid}@127.0.0.1:{server_port}?encryption=none&flow=xtls-rprx-vision&security=reality&sni=www.google.com&fp=chrome&pbk={public_key}&sid=6ba85179e30d4fc2&type=tcp#e2e"
    );
    let (_, cfg) = parse_vless(&link).unwrap();
    let profile = parse_deeplink(include_str!("../tests/fixtures/DEFAULT.DEEPLINK")).unwrap();
    let direct = reqwest::Client::builder().no_proxy().build().unwrap();

    for mode in [RoutingMode::Global, RoutingMode::Smart] {
        let (socks, http, metrics) = (free_port(), free_port(), free_port());
        let client_cfg = build_main(&MainInput {
            server: &cfg,
            server_address: None,
            mode,
            profile: &profile,
            socks_port: socks,
            http_port: http,
            allow_lan: false,
            metrics_port: metrics,
            log_level: "warning",
            ru_direct: false,
            ru_cloud_ip: false,
            proxy_rules: &[],
            direct_rules: &[],
        });
        let _client = spawn(&client_cfg, &format!("client-{mode:?}"));
        wait_port(socks);
        wait_port(metrics);

        let through = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{socks}")).unwrap())
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();

        let before = counters(&direct, metrics).await;
        let resp = through.get("https://www.gstatic.com/generate_204").send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 204, "{mode:?}: tunnel request failed");
        let after_foreign = counters(&direct, metrics).await;
        assert!(after_foreign.0 > before.0, "{mode:?}: foreign site did not go through proxy");

        let resp = through.get("https://ya.ru/").send().await.unwrap();
        assert!(resp.status().as_u16() < 500, "{mode:?}: ya.ru failed");
        let _ = resp.bytes().await;
        let after_ru = counters(&direct, metrics).await;
        match mode {
            RoutingMode::Smart => {
                assert!(after_ru.1 > after_foreign.1, "smart: ya.ru should go direct");
                assert_eq!(after_ru.0, after_foreign.0, "smart: ya.ru leaked into proxy");
            }
            RoutingMode::Global => assert!(after_ru.0 > after_foreign.0, "global: ya.ru should go through proxy"),
        }
        println!("{mode:?}: ok, proxy↓ {} B, direct↓ {} B", after_ru.0, after_ru.1);
    }
}
