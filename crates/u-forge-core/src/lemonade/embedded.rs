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

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use super::{
    LemonadeConnection, LemonadeHttpClient, LemonadeOwnership, LemonadeTimeouts,
    unload_all_models_with_connection,
};

const PORT_START: u16 = 13305;
const PORT_END: u16 = 13315;
const MAX_DIAGNOSTIC_LINES: usize = 200;
#[cfg(unix)]
const TERMINATE_SIGNAL: i32 = libc::SIGTERM;
#[cfg(unix)]
const KILL_SIGNAL: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const TERMINATE_SIGNAL: i32 = 0;
#[cfg(not(unix))]
const KILL_SIGNAL: i32 = 0;

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
    process_group_id: Option<u32>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    data_root: PathBuf,
    shutting_down: Arc<AtomicBool>,
    shutdown_complete: AtomicBool,
    shutdown_lock: tokio::sync::Mutex<()>,
}

impl EmbeddedLemonade {
    pub async fn launch() -> Result<Arc<Self>> {
        let binary = embedded_binary_path()?;
        let data_root = private_cache_root()?;
        migrate_legacy_cache(&data_root)?;
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
        let total_start = std::time::Instant::now();
        if !binary.is_file() {
            return Err(EmbeddedRuntimeError::MissingArtifact(binary).into());
        }
        let package_root = binary.parent().context("lemond path has no parent")?;
        let prepare_start = std::time::Instant::now();
        migrate_packaged_models(package_root, &data_root)?;
        prepare_private_root(package_root, &data_root)?;
        let prepare_duration_us = prepare_start.elapsed().as_micros() as u64;

        let api_key = std::env::var("LEMONADE_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())
            .unwrap_or_else(random_secret);
        let admin_key = std::env::var("LEMONADE_ADMIN_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())
            .unwrap_or_else(random_secret);
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));

        let mut port_probe_duration_us = 0_u64;
        for port in ports {
            let port_start = std::time::Instant::now();
            if !port_is_available(port).await {
                port_probe_duration_us += port_start.elapsed().as_micros() as u64;
                continue;
            }
            port_probe_duration_us += port_start.elapsed().as_micros() as u64;
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
            configure_process_group(&mut command);
            let spawn_start = std::time::Instant::now();
            let mut child = command
                .spawn()
                .with_context(|| format!("failed to launch {}", binary.display()))?;
            let process_group_id = owned_process_group_id(&child);
            let spawn_duration_us = spawn_start.elapsed().as_micros() as u64;
            if let Some(stdout) = child.stdout.take() {
                capture_output(
                    stdout,
                    diagnostics.clone(),
                    "stdout",
                    api_key.clone(),
                    admin_key.clone(),
                );
            }
            if let Some(stderr) = child.stderr.take() {
                capture_output(
                    stderr,
                    diagnostics.clone(),
                    "stderr",
                    api_key.clone(),
                    admin_key.clone(),
                );
            }
            let child = Arc::new(tokio::sync::Mutex::new(Some(child)));
            let readiness_start = std::time::Instant::now();
            match wait_until_ready(connection.clone(), child.clone()).await {
                Ok(()) => {
                    tracing::info!(
                        port,
                        prepare_duration_us,
                        port_probe_duration_us,
                        spawn_duration_us,
                        readiness_duration_us = readiness_start.elapsed().as_micros() as u64,
                        duration_us = total_start.elapsed().as_micros() as u64,
                        "Embedded Lemonade launched"
                    );
                    let shutting_down = Arc::new(AtomicBool::new(false));
                    monitor_child_exit(child.clone(), diagnostics.clone(), shutting_down.clone());
                    return Ok(Arc::new(Self {
                        connection,
                        child,
                        process_group_id,
                        diagnostics,
                        data_root,
                        shutting_down,
                        shutdown_complete: AtomicBool::new(false),
                        shutdown_lock: tokio::sync::Mutex::new(()),
                    }));
                }
                Err(error) => {
                    push_diagnostic(&diagnostics, format!("port {port}: {error:#}"));
                    terminate_child(&child, process_group_id).await;
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
        let _shutdown = self.shutdown_lock.lock().await;
        if self.shutdown_complete.load(Ordering::Acquire) {
            return;
        }
        self.shutting_down.store(true, Ordering::Release);
        shutdown_parts(
            self.connection.clone(),
            self.child.clone(),
            self.process_group_id,
        )
        .await;
        self.shutdown_complete.store(true, Ordering::Release);
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
        if self.shutdown_complete.load(Ordering::Acquire) {
            return;
        }
        let connection = self.connection.clone();
        let child = self.child.clone();
        let process_group_id = self.process_group_id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { shutdown_parts(connection, child, process_group_id).await });
        } else if let Ok(mut child) = self.child.try_lock()
            && let Some(child) = child.as_mut()
        {
            terminate_process_group(process_group_id, KILL_SIGNAL);
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
    process_group_id: Option<u32>,
) {
    shutdown_parts_with_timeouts(
        connection,
        child,
        process_group_id,
        Duration::from_secs(5),
        Duration::from_secs(10),
        Duration::from_secs(5),
    )
    .await;
}

async fn shutdown_parts_with_timeouts(
    connection: Arc<LemonadeConnection>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    process_group_id: Option<u32>,
    admin_timeout: Duration,
    graceful_timeout: Duration,
    kill_timeout: Duration,
) {
    if let Err(error) = tokio::time::timeout(
        admin_timeout,
        unload_all_models_with_connection(connection.clone()),
    )
    .await
    .context("timed out unloading embedded Lemonade models")
    .and_then(|result| result)
    {
        tracing::warn!(%error, "Could not unload all embedded Lemonade models before shutdown");
    }

    let client = LemonadeHttpClient::from_connection(connection);
    if let Err(error) = tokio::time::timeout(
        admin_timeout,
        client.post_admin_empty("/shutdown", &serde_json::json!({})),
    )
    .await
    .context("timed out requesting embedded Lemonade shutdown")
    .and_then(|result| result)
    {
        tracing::warn!(%error, "Embedded Lemonade did not accept graceful shutdown");
    }
    let mut guard = child.lock().await;
    let Some(mut process) = guard.take() else {
        terminate_process_group(process_group_id, KILL_SIGNAL);
        return;
    };
    terminate_process_tree(
        &mut process,
        process_group_id,
        graceful_timeout,
        kill_timeout,
    )
    .await;
}

async fn terminate_child(
    child: &Arc<tokio::sync::Mutex<Option<Child>>>,
    process_group_id: Option<u32>,
) {
    let mut guard = child.lock().await;
    if let Some(mut process) = guard.take() {
        terminate_process_group(process_group_id, KILL_SIGNAL);
        let _ = process.start_kill();
        let _ = process.wait().await;
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn owned_process_group_id(child: &Child) -> Option<u32> {
    child.id()
}

#[cfg(not(unix))]
fn owned_process_group_id(_child: &Child) -> Option<u32> {
    None
}

#[cfg(unix)]
fn terminate_process_group(process_group_id: Option<u32>, signal: i32) {
    let Some(process_group_id) = process_group_id.and_then(|id| i32::try_from(id).ok()) else {
        return;
    };
    // The child is created as the leader of a private process group. A negative
    // PID addresses that group and cannot include the u-forge parent process.
    unsafe {
        libc::kill(-process_group_id, signal);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_process_group_id: Option<u32>, _signal: i32) {}

#[cfg(unix)]
fn process_group_exists(process_group_id: Option<u32>) -> bool {
    let Some(process_group_id) = process_group_id.and_then(|id| i32::try_from(id).ok()) else {
        return false;
    };
    // Signal 0 performs existence/permission checking without changing state.
    unsafe { libc::kill(-process_group_id, 0) == 0 }
}

#[cfg(not(unix))]
fn process_group_exists(_process_group_id: Option<u32>) -> bool {
    false
}

async fn terminate_process_tree(
    process: &mut Child,
    process_group_id: Option<u32>,
    graceful_timeout: Duration,
    kill_timeout: Duration,
) {
    let direct_child_exited = tokio::time::timeout(graceful_timeout, process.wait())
        .await
        .is_ok();

    // A graceful lemond exit does not prove that model backend grandchildren
    // also exited. Terminate the private group and give every descendant a
    // bounded opportunity to clean up before escalating.
    terminate_process_group(process_group_id, TERMINATE_SIGNAL);
    if process_group_exists(process_group_id) {
        tokio::time::sleep(std::cmp::min(kill_timeout, Duration::from_millis(250))).await;
    }
    if process_group_exists(process_group_id) {
        terminate_process_group(process_group_id, KILL_SIGNAL);
    }
    if !direct_child_exited {
        let _ = process.start_kill();
        let _ = tokio::time::timeout(kill_timeout, process.wait()).await;
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
    embedded_binary_path_for(&executable)
}

fn embedded_binary_path_for(executable: &Path) -> Result<PathBuf> {
    let executable_dir = executable
        .parent()
        .context("u-forge executable has no parent")?;
    let profile_dir = match executable_dir.file_name().and_then(|name| name.to_str()) {
        Some("deps" | "examples") => executable_dir
            .parent()
            .context("Cargo executable directory has no profile parent")?,
        _ => executable_dir,
    };
    let cargo_layout = profile_dir.join("lemonade/lemond");
    if cargo_layout.is_file() {
        return Ok(cargo_layout);
    }
    let appdir_layout = executable_dir.join("../lib/u-forge/lemonade/lemond");
    if appdir_layout.is_file() {
        return Ok(appdir_layout);
    }
    // Preserve the historical path in diagnostics when neither packaged
    // layout is present.
    Ok(cargo_layout)
}

fn private_cache_root() -> Result<PathBuf> {
    let base = absolute_env_path("XDG_CACHE_HOME")
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| anyhow!("cannot determine per-user cache directory"))?;
    Ok(base.join("u-forge/lemonade"))
}

fn legacy_data_root() -> Option<PathBuf> {
    absolute_env_path("XDG_DATA_HOME")
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|base| base.join("u-forge/lemonade"))
}

fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn migrate_legacy_cache(cache_root: &Path) -> Result<()> {
    let Some(legacy_root) = legacy_data_root().filter(|legacy| legacy != cache_root) else {
        return Ok(());
    };
    if !legacy_root.is_dir() {
        return Ok(());
    }
    if !cache_root.exists() {
        std::fs::create_dir_all(
            cache_root
                .parent()
                .context("embedded cache root has no parent")?,
        )?;
        std::fs::rename(&legacy_root, cache_root).with_context(|| {
            format!(
                "failed to move the legacy embedded cache from {} to XDG path {}",
                legacy_root.display(),
                cache_root.display()
            )
        })?;
        return Ok(());
    }
    move_missing_entries(&legacy_root, cache_root)?;
    Ok(())
}

fn migrate_packaged_models(package_root: &Path, cache_root: &Path) -> Result<()> {
    let legacy_models = package_root.join("models");
    if !legacy_models.is_dir() {
        return Ok(());
    }
    let models = cache_root.join("models");
    std::fs::create_dir_all(&models)?;
    move_missing_entries(&legacy_models, &models).with_context(|| {
        format!(
            "failed to move models from {} to XDG cache {}",
            legacy_models.display(),
            models.display()
        )
    })
}

fn move_missing_entries(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if target.exists() {
            continue;
        }
        std::fs::rename(entry.path(), &target).with_context(|| {
            format!(
                "failed to move cache entry {} to {}",
                entry.path().display(),
                target.display()
            )
        })?;
    }
    if std::fs::read_dir(source)?.next().is_none() {
        std::fs::remove_dir(source)?;
    }
    Ok(())
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
        object.insert(
            "models_dir".into(),
            serde_json::json!(data_root.join("models")),
        );
        let telemetry = object
            .entry("telemetry")
            .or_insert_with(|| serde_json::json!({}));
        let telemetry = telemetry
            .as_object_mut()
            .ok_or_else(|| anyhow!("Lemonade telemetry defaults must contain an object"))?;
        telemetry.insert("enabled".into(), serde_json::json!(false));
        atomic_write(&config, &serde_json::to_vec_pretty(&value)?)?;
    } else {
        migrate_relative_models_dir(&config, data_root)?;
    }
    Ok(())
}

fn migrate_relative_models_dir(config: &Path, cache_root: &Path) -> Result<()> {
    let bytes = std::fs::read(config)?;
    let mut value = serde_json::from_slice::<serde_json::Value>(&bytes)?;
    if value.get("models_dir").and_then(serde_json::Value::as_str) != Some("./models") {
        return Ok(());
    }
    value
        .as_object_mut()
        .context("Lemonade config.json must contain an object")?
        .insert(
            "models_dir".into(),
            serde_json::json!(cache_root.join("models")),
        );
    let mut updated = serde_json::to_vec_pretty(&value)?;
    updated.push(b'\n');
    atomic_write(config, &updated)
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
    stream: &'static str,
    api: String,
    admin: String,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line
                .replace(&api, "<redacted>")
                .replace(&admin, "<redacted>");
            tracing::debug!(
                target: "u_forge_core::lemonade::embedded::lemond",
                stream,
                message = %line,
                "embedded Lemonade output"
            );
            push_diagnostic(&diagnostics, line);
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    fn fake_binary(
        package: &Path,
        ignore_shutdown: bool,
        exit_after_live: bool,
        descendant_pid_file: Option<&Path>,
    ) -> PathBuf {
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
        let descendant = descendant_pid_file
            .map(|path| {
                let quoted = path.display().to_string().replace('\'', "'\"'\"'");
                format!("sleep 300 &\nprintf '%s' \"$!\" > '{quoted}'\n")
            })
            .unwrap_or_default();
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nport=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--port\" ]; then port=\"$2\"; shift 2; else shift; fi\ndone\nexport UFORGE_FAKE_LEMOND_PORT=\"$port\"\n{ignore}{exit}{descendant}exec '{quoted}' --exact lemonade::embedded::tests::fake_lemond_process --nocapture\n"
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
        assert_eq!(
            config["models_dir"],
            data.path().join("models").to_string_lossy().as_ref()
        );
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
    fn legacy_relative_model_path_migrates_without_rewriting_other_settings() {
        let cache = tempfile::tempdir().unwrap();
        let config = cache.path().join("config.json");
        std::fs::write(&config, br#"{"models_dir":"./models","preserve":true}"#).unwrap();

        migrate_relative_models_dir(&config, cache.path()).unwrap();

        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
        assert_eq!(
            value["models_dir"],
            cache.path().join("models").to_string_lossy().as_ref()
        );
        assert_eq!(value["preserve"], true);
    }

    #[test]
    fn cache_migration_moves_only_entries_missing_at_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("legacy");
        let destination = temp.path().join("xdg-cache");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("move-me"), b"legacy").unwrap();
        std::fs::write(source.join("preserve-me"), b"legacy").unwrap();
        std::fs::write(destination.join("preserve-me"), b"current").unwrap();

        move_missing_entries(&source, &destination).unwrap();

        assert_eq!(
            std::fs::read(destination.join("move-me")).unwrap(),
            b"legacy"
        );
        assert_eq!(
            std::fs::read(destination.join("preserve-me")).unwrap(),
            b"current"
        );
        assert_eq!(
            std::fs::read(source.join("preserve-me")).unwrap(),
            b"legacy"
        );
    }

    #[test]
    fn embedded_artifact_is_resolved_from_application_and_cargo_subdirectories() {
        assert_eq!(
            embedded_binary_path_for(Path::new("/workspace/target/debug/u-forge")).unwrap(),
            PathBuf::from("/workspace/target/debug/lemonade/lemond")
        );
        for cargo_subdirectory in ["examples", "deps"] {
            assert_eq!(
                embedded_binary_path_for(
                    Path::new("/workspace/target/debug")
                        .join(cargo_subdirectory)
                        .join("runner")
                        .as_path(),
                )
                .unwrap(),
                PathBuf::from("/workspace/target/debug/lemonade/lemond")
            );
        }
    }

    #[test]
    fn embedded_artifact_is_resolved_from_appdir_library() {
        let temp = tempfile::tempdir().unwrap();
        let appdir = temp.path().join("u-forge.AppDir/usr");
        let lemond = appdir.join("lib/u-forge/lemonade/lemond");
        std::fs::create_dir_all(lemond.parent().unwrap()).unwrap();
        std::fs::create_dir_all(appdir.join("bin")).unwrap();
        std::fs::write(&lemond, "").unwrap();

        assert_eq!(
            embedded_binary_path_for(&appdir.join("bin/u-forge")).unwrap(),
            appdir.join("bin/../lib/u-forge/lemonade/lemond")
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
            "stdout",
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
        let binary = fake_binary(package.path(), false, false, None);
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
        let binary = fake_binary(package.path(), true, false, None);
        let mut timeouts = LemonadeTimeouts::default();
        timeouts.readiness_load = Duration::from_secs(3);
        let embedded =
            EmbeddedLemonade::launch_from(binary, data.path().to_path_buf(), [port], timeouts)
                .await
                .unwrap();
        shutdown_parts_with_timeouts(
            embedded.connection.clone(),
            embedded.child.clone(),
            embedded.process_group_id,
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
    async fn shutdown_unloads_all_models_before_requesting_server_exit() {
        let Some((port, _)) = two_available_ports() else {
            eprintln!("SKIP: embedded test port is not available");
            return;
        };
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut request_lines = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0_u8; 4096];
                let read = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..read]);
                request_lines.push(request.lines().next().unwrap_or_default().to_string());
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
            }
            request_lines
        });
        let connection = Arc::new(
            LemonadeConnection::with_credentials(
                &format!("http://{address}/v1"),
                LemonadeOwnership::Embedded,
                None,
                None,
                LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let child = Arc::new(tokio::sync::Mutex::new(None));

        shutdown_parts_with_timeouts(
            connection,
            child,
            None,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await;

        assert_eq!(
            server.await.unwrap(),
            [
                "POST /v1/unload HTTP/1.1",
                "POST /internal/shutdown HTTP/1.1",
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shutdown_terminates_backend_descendants() {
        let Some((port, _)) = two_available_ports() else {
            eprintln!("SKIP: embedded test port is not available");
            return;
        };
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let descendant_pid_file = data.path().join("descendant.pid");
        let binary = fake_binary(package.path(), false, false, Some(&descendant_pid_file));
        let mut timeouts = LemonadeTimeouts::default();
        timeouts.readiness_load = Duration::from_secs(3);
        let embedded =
            EmbeddedLemonade::launch_from(binary, data.path().to_path_buf(), [port], timeouts)
                .await
                .unwrap();

        let descendant_pid = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&descendant_pid_file)
                    && let Ok(pid) = pid.parse::<u32>()
                {
                    break pid;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(linux_process_is_running(descendant_pid));

        embedded.shutdown().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while linux_process_is_running(descendant_pid) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("backend descendant survived embedded shutdown");
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
        let binary = fake_binary(package.path(), false, true, None);
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
            embedded.process_group_id,
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

    #[cfg(target_os = "linux")]
    fn linux_process_is_running(pid: u32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.chars().next())
            .is_some_and(|state| state != 'Z')
    }
}
