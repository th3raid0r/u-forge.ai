//! Owned Embeddable Lemonade process lifecycle.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    process::{Child, Command},
};
use uuid::Uuid;

use super::{LemonadeConnection, LemonadeHttpClient, LemonadeOwnership, LemonadeTimeouts};

const PORT_START: u16 = 13305;
const PORT_END: u16 = 13315;
const MAX_DIAGNOSTIC_LINES: usize = 200;

#[derive(Debug, Error)]
pub enum EmbeddedRuntimeError {
    #[error("embedded Lemonade artifact is unavailable at {0}")]
    MissingArtifact(PathBuf),
    #[error("no private Lemonade port was available in 13305..=13315")]
    NoPort,
    #[error("embedded Lemonade failed to become ready: {0}")]
    Readiness(String),
}

/// Handle for one process that u-forge itself launched.
pub struct EmbeddedLemonade {
    connection: Arc<LemonadeConnection>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    data_root: PathBuf,
    shutting_down: Arc<AtomicBool>,
}

impl EmbeddedLemonade {
    pub async fn launch() -> Result<Arc<Self>> {
        let binary = embedded_binary_path()?;
        let data_root = private_data_root()?;
        Self::launch_from(
            binary,
            data_root,
            PORT_START..=PORT_END,
            LemonadeTimeouts::default(),
        )
        .await
    }

    async fn launch_from(
        binary: PathBuf,
        data_root: PathBuf,
        ports: impl IntoIterator<Item = u16>,
        timeouts: LemonadeTimeouts,
    ) -> Result<Arc<Self>> {
        if !binary.is_file() {
            return Err(EmbeddedRuntimeError::MissingArtifact(binary).into());
        }
        let package_root = binary.parent().context("lemond path has no parent")?;
        prepare_private_root(package_root, &data_root)?;

        let api_key = std::env::var("LEMONADE_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())
            .unwrap_or_else(random_secret);
        let admin_key = std::env::var("LEMONADE_ADMIN_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())
            .unwrap_or_else(random_secret);
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));

        for port in ports {
            if !port_is_available(port).await {
                continue;
            }
            let connection = Arc::new(LemonadeConnection::with_credentials(
                &format!("http://127.0.0.1:{port}/v1"),
                LemonadeOwnership::Embedded,
                Some(api_key.clone()),
                Some(admin_key.clone()),
                timeouts,
            )?);
            let mut command = embedded_command(
                &binary,
                &data_root,
                port,
                api_key.as_str(),
                admin_key.as_str(),
            );
            let mut child = command
                .spawn()
                .with_context(|| format!("failed to launch {}", binary.display()))?;
            if let Some(stdout) = child.stdout.take() {
                capture_output(
                    stdout,
                    diagnostics.clone(),
                    api_key.clone(),
                    admin_key.clone(),
                );
            }
            if let Some(stderr) = child.stderr.take() {
                capture_output(
                    stderr,
                    diagnostics.clone(),
                    api_key.clone(),
                    admin_key.clone(),
                );
            }
            let child = Arc::new(tokio::sync::Mutex::new(Some(child)));
            match wait_until_ready(connection.clone(), child.clone()).await {
                Ok(()) => {
                    let shutting_down = Arc::new(AtomicBool::new(false));
                    monitor_child_exit(child.clone(), diagnostics.clone(), shutting_down.clone());
                    return Ok(Arc::new(Self {
                        connection,
                        child,
                        diagnostics,
                        data_root,
                        shutting_down,
                    }));
                }
                Err(error) => {
                    push_diagnostic(&diagnostics, format!("port {port}: {error:#}"));
                    terminate_child(&child).await;
                }
            }
        }
        if diagnostics.lock().is_empty() {
            Err(EmbeddedRuntimeError::NoPort.into())
        } else {
            Err(EmbeddedRuntimeError::Readiness(
                diagnostics
                    .lock()
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .into())
        }
    }

    pub fn connection(&self) -> Arc<LemonadeConnection> {
        self.connection.clone()
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics.lock().iter().cloned().collect()
    }

    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        shutdown_parts(self.connection.clone(), self.child.clone()).await;
    }
}

fn embedded_command(
    binary: &Path,
    data_root: &Path,
    port: u16,
    api_key: &str,
    admin_key: &str,
) -> Command {
    let mut command = Command::new(binary);
    command
        .arg(data_root)
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg("127.0.0.1")
        .env("LEMONADE_API_KEY", api_key)
        .env("LEMONADE_ADMIN_API_KEY", admin_key)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

impl Drop for EmbeddedLemonade {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        let connection = self.connection.clone();
        let child = self.child.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { shutdown_parts(connection, child).await });
        } else if let Ok(mut child) = self.child.try_lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.start_kill();
        }
    }
}

fn monitor_child_exit(
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    shutting_down: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let status = {
                let mut guard = child.lock().await;
                let Some(process) = guard.as_mut() else {
                    return;
                };
                process.try_wait()
            };
            match status {
                Ok(Some(status)) => {
                    if !shutting_down.load(Ordering::Acquire) {
                        push_diagnostic(
                            &diagnostics,
                            format!("embedded Lemonade exited unexpectedly with {status}"),
                        );
                    }
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    if !shutting_down.load(Ordering::Acquire) {
                        push_diagnostic(
                            &diagnostics,
                            format!("failed to inspect embedded Lemonade child: {error}"),
                        );
                    }
                    return;
                }
            }
        }
    });
}

async fn wait_until_ready(
    connection: Arc<LemonadeConnection>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + connection.timeouts().readiness_load;
    let client = LemonadeHttpClient::from_connection(connection);
    loop {
        {
            let mut guard = child.lock().await;
            if let Some(status) = guard
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten()
            {
                return Err(anyhow!("lemond exited before readiness with {status}"));
            }
        }
        if client
            .get_origin_json::<serde_json::Value>("/live")
            .await
            .is_ok()
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for /live"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn shutdown_parts(
    connection: Arc<LemonadeConnection>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
) {
    shutdown_parts_with_timeouts(
        connection,
        child,
        Duration::from_secs(5),
        Duration::from_secs(10),
        Duration::from_secs(5),
    )
    .await;
}

async fn shutdown_parts_with_timeouts(
    connection: Arc<LemonadeConnection>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    admin_timeout: Duration,
    graceful_timeout: Duration,
    kill_timeout: Duration,
) {
    let client = LemonadeHttpClient::from_connection(connection);
    let _ = tokio::time::timeout(
        admin_timeout,
        client.post_admin_empty("/shutdown", &serde_json::json!({})),
    )
    .await;
    let mut guard = child.lock().await;
    let Some(mut process) = guard.take() else {
        return;
    };
    if tokio::time::timeout(graceful_timeout, process.wait())
        .await
        .is_err()
    {
        let _ = process.start_kill();
        let _ = tokio::time::timeout(kill_timeout, process.wait()).await;
    }
}

async fn terminate_child(child: &Arc<tokio::sync::Mutex<Option<Child>>>) {
    let mut guard = child.lock().await;
    if let Some(mut process) = guard.take() {
        let _ = process.start_kill();
        let _ = process.wait().await;
    }
}

async fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).await.is_ok()
}

fn embedded_binary_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("UFORGE_LEMOND_PATH") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe().context("failed to locate u-forge executable")?;
    Ok(executable
        .parent()
        .context("u-forge executable has no parent")?
        .join("lemonade/lemond"))
}

fn private_data_root() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| anyhow!("cannot determine per-user data directory"))?;
    Ok(base.join("u-forge/lemonade"))
}

fn prepare_private_root(package_root: &Path, data_root: &Path) -> Result<()> {
    std::fs::create_dir_all(data_root.join("resources"))?;
    std::fs::create_dir_all(data_root.join("models"))?;
    for name in [
        "server_models.json",
        "backend_versions.json",
        "defaults.json",
    ] {
        let source = package_root.join("resources").join(name);
        if source.is_file() {
            atomic_copy(&source, &data_root.join("resources").join(name))?;
        }
    }
    let config = data_root.join("config.json");
    if !config.exists() {
        let defaults = data_root.join("resources/defaults.json");
        let mut value = if defaults.is_file() {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(defaults)?)?
        } else {
            serde_json::json!({})
        };
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow!("Lemonade defaults.json must contain an object"))?;
        object.insert("host".into(), serde_json::json!("127.0.0.1"));
        object.insert("no_broadcast".into(), serde_json::json!(true));
        object.insert("models_dir".into(), serde_json::json!("./models"));
        let telemetry = object
            .entry("telemetry")
            .or_insert_with(|| serde_json::json!({}));
        let telemetry = telemetry
            .as_object_mut()
            .ok_or_else(|| anyhow!("Lemonade telemetry defaults must contain an object"))?;
        telemetry.insert("enabled".into(), serde_json::json!(false));
        atomic_write(&config, &serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(())
}

fn atomic_copy(source: &Path, destination: &Path) -> Result<()> {
    let bytes = std::fs::read(source)?;
    if std::fs::read(destination).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    atomic_write(destination, &bytes)
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> Result<()> {
    let temp = destination.with_extension(format!("tmp-{}", Uuid::new_v4()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, destination)?;
    Ok(())
}

fn random_secret() -> String {
    Uuid::new_v4().simple().to_string()
}

fn capture_output<R>(
    reader: R,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    api: String,
    admin: String,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            push_diagnostic(
                &diagnostics,
                line.replace(&api, "<redacted>")
                    .replace(&admin, "<redacted>"),
            );
        }
    });
}

fn push_diagnostic(diagnostics: &Mutex<VecDeque<String>>, line: String) {
    let mut diagnostics = diagnostics.lock();
    if diagnostics.len() == MAX_DIAGNOSTIC_LINES {
        diagnostics.pop_front();
    }
    diagnostics.push_back(line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn fake_lemond_process() {
        let Some(port) = std::env::var("UFORGE_FAKE_LEMOND_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
        else {
            return;
        };
        let ignore_shutdown = std::env::var_os("UFORGE_FAKE_IGNORE_SHUTDOWN").is_some();
        let exit_after_live = std::env::var_os("UFORGE_FAKE_EXIT_AFTER_LIVE").is_some();
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        for socket in listener.incoming() {
            let mut socket = socket.unwrap();
            let mut bytes = [0_u8; 4096];
            let read = socket.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..read]);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
            if request.contains("/internal/shutdown") && !ignore_shutdown {
                break;
            }
            if request.contains("/live") && exit_after_live {
                break;
            }
        }
    }

    #[cfg(unix)]
    fn fake_binary(package: &Path, ignore_shutdown: bool, exit_after_live: bool) -> PathBuf {
        std::fs::create_dir_all(package).unwrap();
        let binary = package.join("lemond");
        let test_binary = std::env::current_exe().unwrap();
        let quoted = test_binary.display().to_string().replace('\'', "'\"'\"'");
        let ignore = if ignore_shutdown {
            "export UFORGE_FAKE_IGNORE_SHUTDOWN=1\n"
        } else {
            ""
        };
        let exit = if exit_after_live {
            "export UFORGE_FAKE_EXIT_AFTER_LIVE=1\n"
        } else {
            ""
        };
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nport=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--port\" ]; then port=\"$2\"; shift 2; else shift; fi\ndone\nexport UFORGE_FAKE_LEMOND_PORT=\"$port\"\n{ignore}{exit}exec '{quoted}' --exact lemonade::embedded::tests::fake_lemond_process --nocapture\n"
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
        binary
    }

    #[cfg(unix)]
    fn two_available_ports() -> Option<(u16, u16)> {
        let mut ports = Vec::new();
        for port in PORT_START..=PORT_END {
            if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                drop(listener);
                ports.push(port);
                if ports.len() == 2 {
                    return Some((ports[0], ports[1]));
                }
            }
        }
        None
    }

    #[test]
    fn private_root_seeds_owned_defaults_without_overwriting_config() {
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir(package.path().join("resources")).unwrap();
        std::fs::write(package.path().join("resources/defaults.json"), b"{}").unwrap();
        prepare_private_root(package.path(), data.path()).unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data.path().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(config["host"], "127.0.0.1");
        assert_eq!(config["models_dir"], "./models");
        assert_eq!(config["telemetry"]["enabled"], false);
        std::fs::write(data.path().join("config.json"), b"{\"preserve\":true}").unwrap();
        std::fs::write(
            data.path().join("recipe_options.json"),
            b"{\"recipe\":true}",
        )
        .unwrap();
        std::fs::write(data.path().join("user_models.json"), b"{\"user\":true}").unwrap();
        prepare_private_root(package.path(), data.path()).unwrap();
        assert_eq!(
            std::fs::read(data.path().join("config.json")).unwrap(),
            b"{\"preserve\":true}"
        );
        assert_eq!(
            std::fs::read(data.path().join("recipe_options.json")).unwrap(),
            b"{\"recipe\":true}"
        );
        assert_eq!(
            std::fs::read(data.path().join("user_models.json")).unwrap(),
            b"{\"user\":true}"
        );
    }

    #[test]
    fn owned_command_uses_only_supported_lemond_arguments() {
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let binary = package.path().join("lemond");
        let command = embedded_command(&binary, data.path(), 13306, "api", "admin");
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                data.path().to_string_lossy().into_owned(),
                "--port".to_owned(),
                "13306".to_owned(),
                "--host".to_owned(),
                "127.0.0.1".to_owned(),
            ]
        );
        assert!(
            !arguments
                .iter()
                .any(|argument| argument == "--no-broadcast")
        );
    }

    #[tokio::test]
    async fn captured_diagnostics_are_redacted_and_bounded() {
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let (reader, mut writer) = tokio::io::duplex(4096);
        capture_output(
            reader,
            diagnostics.clone(),
            "api-secret".to_string(),
            "admin-secret".to_string(),
        );
        writer
            .write_all(b"keys api-secret and admin-secret\n")
            .await
            .unwrap();
        drop(writer);
        tokio::time::timeout(Duration::from_secs(1), async {
            while diagnostics.lock().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let line = diagnostics.lock().front().cloned().unwrap();
        assert_eq!(line, "keys <redacted> and <redacted>");

        for index in 0..MAX_DIAGNOSTIC_LINES + 5 {
            push_diagnostic(&diagnostics, format!("line-{index}"));
        }
        let diagnostics = diagnostics.lock();
        assert_eq!(diagnostics.len(), MAX_DIAGNOSTIC_LINES);
        assert_eq!(diagnostics.back().unwrap(), "line-204");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn owned_process_retries_ports_becomes_ready_and_shuts_down() {
        let Some((occupied_port, launch_port)) = two_available_ports() else {
            eprintln!("SKIP: two embedded test ports are not available");
            return;
        };
        let occupied = std::net::TcpListener::bind(("127.0.0.1", occupied_port)).unwrap();
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let binary = fake_binary(package.path(), false, false);
        let mut timeouts = LemonadeTimeouts::default();
        timeouts.readiness_load = Duration::from_secs(3);
        let embedded = EmbeddedLemonade::launch_from(
            binary,
            data.path().to_path_buf(),
            [occupied_port, launch_port],
            timeouts,
        )
        .await
        .unwrap();
        assert_eq!(
            embedded.connection.origin(),
            format!("http://127.0.0.1:{launch_port}")
        );
        assert_eq!(embedded.connection.ownership(), LemonadeOwnership::Embedded);
        drop(occupied);
        embedded.shutdown().await;
        assert!(embedded.child.lock().await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_escalates_only_the_owned_child() {
        let Some((port, _)) = two_available_ports() else {
            eprintln!("SKIP: embedded test port is not available");
            return;
        };
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let binary = fake_binary(package.path(), true, false);
        let mut timeouts = LemonadeTimeouts::default();
        timeouts.readiness_load = Duration::from_secs(3);
        let embedded =
            EmbeddedLemonade::launch_from(binary, data.path().to_path_buf(), [port], timeouts)
                .await
                .unwrap();
        shutdown_parts_with_timeouts(
            embedded.connection.clone(),
            embedded.child.clone(),
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_secs(1),
        )
        .await;
        assert!(embedded.child.lock().await.is_none());

        let external = LemonadeConnection::external("http://127.0.0.1:1/v1").unwrap();
        assert_eq!(external.ownership(), LemonadeOwnership::External);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unexpected_post_readiness_exit_is_detected() {
        let Some((port, _)) = two_available_ports() else {
            eprintln!("SKIP: embedded test port is not available");
            return;
        };
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let binary = fake_binary(package.path(), false, true);
        let mut timeouts = LemonadeTimeouts::default();
        timeouts.readiness_load = Duration::from_secs(3);
        let embedded =
            EmbeddedLemonade::launch_from(binary, data.path().to_path_buf(), [port], timeouts)
                .await
                .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if embedded
                    .diagnostics()
                    .iter()
                    .any(|line| line.contains("exited unexpectedly"))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        embedded.shutting_down.store(true, Ordering::Release);
        shutdown_parts_with_timeouts(
            embedded.connection.clone(),
            embedded.child.clone(),
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_secs(1),
        )
        .await;
    }

    #[tokio::test]
    async fn missing_owned_artifact_is_a_structured_error() {
        let data = tempfile::tempdir().unwrap();
        let error = EmbeddedLemonade::launch_from(
            data.path().join("missing-lemond"),
            data.path().join("data"),
            [PORT_START],
            LemonadeTimeouts::default(),
        )
        .await
        .err()
        .expect("missing artifact must fail");
        assert!(error.to_string().contains("artifact is unavailable"));
    }
}
