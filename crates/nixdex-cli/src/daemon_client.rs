//! Thin HTTP client for the `nixdex` daemon, with automatic background spawn.
//!
//! `nix-locate` prefers a resident daemon: it queries `/nix-locate` over HTTP so
//! the (mmap-backed) database reader and secondary indexes stay warm across
//! invocations. If no daemon is listening, the client spawns one in the
//! background (paying the spawn+load cost once) and waits for it to become
//! ready. Any failure falls back to a local search in the caller.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// Default daemon listen address (matches `nixdex daemon --http-addr` default).
pub const DEFAULT_ADDR: &str = "127.0.0.1:3750";

/// Override the daemon address via `NIXDEX_DAEMON_ADDR`.
const ADDR_ENV: &str = "NIXDEX_DAEMON_ADDR";
/// Disable daemon use entirely via `NIXDEX_NO_DAEMON=1`.
const NO_DAEMON_ENV: &str = "NIXDEX_NO_DAEMON";

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct NixLocateResponse {
    pub count: Option<usize>,
    pub matches: Vec<NixLocateMatch>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct NixLocateMatch {
    pub attr: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub node: Option<NixLocateNode>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub maintainers: Option<Vec<String>>,
    #[serde(default)]
    pub platforms: Option<Vec<String>>,
    #[serde(default)]
    pub main_program: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(untagged)]
pub enum NixLocateNode {
    Regular { r#type: String, size: u64 },
    Directory { r#type: String, size: u64 },
    Symlink { r#type: String, target: String },
}

/// Errors that mean "use the local search path instead".
#[derive(Debug)]
pub enum DaemonError {
    /// No daemon was reachable and spawning one failed or timed out.
    Unavailable,
    /// The daemon answered but the request/response was unusable.
    Transport(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "no daemon available"),
            Self::Transport(msg) => write!(f, "daemon transport error: {msg}"),
        }
    }
}

pub struct DaemonClient {
    base: String,
    client: reqwest::Client,
}

impl DaemonClient {
    pub fn new(addr: String) -> Self {
        Self {
            base: addr,
            client: reqwest::Client::new(),
        }
    }

    pub async fn ready(&self) -> bool {
        self.client
            .get(format!("http://{}/ready", self.base))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    pub async fn locate(
        &self,
        query: &[(String, String)],
    ) -> Result<NixLocateResponse, DaemonError> {
        let resp = self
            .client
            .get(format!("http://{}/nix-locate", self.base))
            .query(query)
            .send()
            .await
            .map_err(|e| DaemonError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(DaemonError::Transport(format!("status {}", resp.status())));
        }
        resp.json()
            .await
            .map_err(|e| DaemonError::Transport(e.to_string()))
    }
}

/// Spawn the daemon binary (this executable) in the background, detached from
/// the current process group so it outlives the CLI invocation.
fn spawn_daemon(
    database: &Path,
    addr: &str,
    cache_mode: &str,
) -> std::io::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .arg("--database")
        .arg(database)
        .arg("--http-addr")
        .arg(addr)
        .arg("--index-cache-mode")
        .arg(cache_mode)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach into its own process group so it survives the CLI exiting.
        cmd.process_group(0);
    }
    cmd.spawn()
}

/// Return a client for a ready daemon, spawning one (bound to `addr` serving
/// `database`) if none is listening. Waits up to 10s for readiness.
pub async fn ensure_client(database: &Path, addr: &str, cache_mode: &str) -> DaemonClient {
    let client = DaemonClient::new(addr.to_string());
    if client.ready().await {
        return client;
    }
    // Best-effort spawn; if it fails we simply report unavailable and the
    // caller falls back to a local search.
    let _ = spawn_daemon(database, addr, cache_mode);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if client.ready().await {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    client
}

/// Resolve the daemon address to use, honouring `NIXDEX_DAEMON_ADDR`.
#[must_use]
pub fn resolve_addr() -> String {
    std::env::var(ADDR_ENV).unwrap_or_else(|_| DEFAULT_ADDR.to_string())
}

/// Whether daemon use should be skipped entirely.
#[must_use]
pub fn daemon_disabled() -> bool {
    std::env::var(NO_DAEMON_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

fn node_kind_size(node: Option<&NixLocateNode>) -> (&str, u64) {
    match node {
        Some(
            NixLocateNode::Regular { r#type, size } | NixLocateNode::Directory { r#type, size },
        ) => (r#type.as_str(), *size),
        Some(NixLocateNode::Symlink { r#type, .. }) => (r#type.as_str(), 0),
        None => ("?", 0),
    }
}

fn store_path_string(m: &NixLocateMatch) -> String {
    match (m.hash.as_deref(), m.name.as_deref()) {
        (Some(hash), Some(name)) => format!("/nix/store/{hash}-{name}"),
        _ => m.attr.clone(),
    }
}

/// Format an integer with thousands separators (mirrors the core renderer).
fn format_grouped(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        let remaining = bytes.len() - i;
        if i > 0 && remaining.is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(*b));
    }
    out
}

/// Render a daemon response into the same text/JSON/count lines the local
/// search path would print. Colour highlighting is omitted because the daemon
/// response does not carry per-match highlight ranges.
pub(crate) fn render(response: &NixLocateResponse, json: bool, minimal: bool, null_output: bool) -> Vec<String> {
    if let Some(count) = response.count {
        return vec![count.to_string()];
    }

    let delim = if null_output { "\0" } else { "\n" };

    if json {
        response
            .matches
            .iter()
            .map(|m| {
                let (kind, size) = node_kind_size(m.node.as_ref());
                let mut obj = sonic_rs::json!({
                    "attr": m.attr,
                    "size": size,
                    "kind": kind,
                    "path": m.path.clone().unwrap_or_else(String::new),
                    "store_path": store_path_string(m),
                });
                if let Some(ref desc) = m.description {
                    obj.insert("description", sonic_rs::Value::copy_str(desc));
                }
                if let Some(ref lic) = m.license {
                    obj.insert("license", sonic_rs::Value::copy_str(lic));
                }
                if let Some(ref hp) = m.homepage {
                    obj.insert("homepage", sonic_rs::Value::copy_str(hp));
                }
                if let Some(ref maint) = m.maintainers {
                    if let Ok(val) = sonic_rs::to_value(maint) {
                        obj.insert("maintainers", val);
                    }
                }
                if let Some(ref plats) = m.platforms {
                    if let Ok(val) = sonic_rs::to_value(plats) {
                        obj.insert("platforms", val);
                    }
                }
                if let Some(ref mp) = m.main_program {
                    obj.insert("main_program", sonic_rs::Value::copy_str(mp));
                }
                let line = sonic_rs::to_string(&obj).unwrap_or_else(|_| String::new());
                format!("{line}{delim}")
            })
            .collect()
    } else if minimal {
        response
            .matches
            .iter()
            .map(|m| format!("{}{delim}", m.attr))
            .collect()
    } else {
        response
            .matches
            .iter()
            .map(|m| {
                let (kind, size) = node_kind_size(m.node.as_ref());
                let size_str = format_grouped(size);
                let sp = store_path_string(m);
                let path = m.path.clone().unwrap_or_else(String::new);
                format!("{:<40} {:>14} {:>1} {}{}{delim}", m.attr, size_str, kind, sp, path)
            })
            .collect()
    }
}
