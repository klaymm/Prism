use crate::logs::{LogBuffer, parse_line};
use crate::win::KillOnCloseJob;
use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn is_noise(source: &str, msg: &str) -> bool {
    source == "sing-box"
        && (msg.contains("inbound connection")
            || msg.contains("outbound connection")
            || msg.contains("inbound packet connection")
            || msg.contains("outbound packet connection"))
}

fn pipe_lines(stream: impl Read + Send + 'static, source: &'static str, logs: Arc<LogBuffer>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::with_capacity(256);
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    if line.trim().is_empty() {
                        continue;
                    }
                    let (level, msg) = parse_line(&line);
                    if !is_noise(source, &msg) {
                        logs.push(source, level, msg);
                    }
                }
            }
        }
    });
}

pub struct CoreProcess {
    pub source: &'static str,
    child: Child,
}

impl CoreProcess {
    pub fn spawn(
        source: &'static str,
        exe: &Path,
        args: &[&str],
        envs: &[(&str, &Path)],
        cwd: &Path,
        logs: &Arc<LogBuffer>,
        job: Option<&KillOnCloseJob>,
    ) -> Result<Self, String> {
        if !exe.exists() {
            return Err(format!("не найдено ядро {}", exe.display()));
        }
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("не удалось запустить {source}: {e}"))?;
        if let Some(job) = job {
            job.assign(&child);
        }
        if let Some(out) = child.stdout.take() {
            pipe_lines(out, source, logs.clone());
        }
        if let Some(err) = child.stderr.take() {
            pipe_lines(err, source, logs.clone());
        }
        Ok(Self { source, child })
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for CoreProcess {
    fn drop(&mut self) {
        if self.is_running() {
            self.kill();
        }
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(0)
}

pub fn port_available(listen: &str, port: u16) -> bool {
    std::net::TcpListener::bind((listen, port)).is_ok()
}

fn exit_error(logs: &LogBuffer, since: u64, source: &str) -> String {
    match logs.last_error_since(since, source) {
        Some(msg) => format!("{source} завершился: {msg}"),
        None => format!("{source} завершился сразу после запуска"),
    }
}

pub async fn wait_for_port(
    proc: &mut CoreProcess,
    port: u16,
    timeout: Duration,
    logs: &LogBuffer,
    since: u64,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if !proc.is_running() {
            return Err(exit_error(logs, since, proc.source));
        }
        let probe = tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await;
        if matches!(probe, Ok(Ok(_))) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("{} не открыл порт {port} вовремя", proc.source));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub const CANCELLED: &str = "cancelled";

pub async fn wait_for_log(
    proc: &mut CoreProcess,
    needle: &str,
    timeout: Duration,
    logs: &LogBuffer,
    since: u64,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled() {
            return Err(CANCELLED.into());
        }
        if !proc.is_running() {
            return Err(exit_error(logs, since, proc.source));
        }
        if logs.find_since(since, proc.source, |e| e.message.contains(needle)).is_some() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("{} не запустился вовремя", proc.source));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
