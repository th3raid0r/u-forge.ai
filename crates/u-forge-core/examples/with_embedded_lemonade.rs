//! Run one command against a single owned Embeddable Lemonade instance.
//!
//! The root Makefile uses this process boundary to keep the server alive across
//! every Cargo test binary while still guaranteeing an awaited shutdown.

use std::{env, ffi::OsString, process::ExitStatus, time::Duration};

use anyhow::{Context, Result, anyhow};
use u_forge_core::lemonade::EmbeddedLemonade;

#[tokio::main]
async fn main() {
    let exit_code = match run().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("embedded Lemonade test runner failed: {error:#}");
            1
        }
    };
    std::process::exit(exit_code);
}

async fn run() -> Result<i32> {
    let mut arguments = env::args_os().skip(1);
    let program = arguments
        .next()
        .ok_or_else(|| anyhow!("usage: with_embedded_lemonade <command> [arguments...]"))?;
    let arguments = arguments.collect::<Vec<OsString>>();

    let embedded = EmbeddedLemonade::launch()
        .await
        .context("failed to launch the checksum-pinned embedded runtime")?;
    let connection = embedded.connection();
    eprintln!(
        "test runtime: one embedded Lemonade server at {}",
        connection.api_base()
    );

    let mut command = tokio::process::Command::new(&program);
    command
        .args(arguments)
        .env("LEMONADE_URL", connection.api_base())
        .env("UFORGE_INTEGRATION_TESTS", "require")
        .env("UFORGE_REQUIRE_EMBEDDED_LEMONADE", "1")
        .env_remove("UFORGE_SKIP_EMBEDDED_LEMONADE")
        .kill_on_drop(true);
    if let Some(api_key) = connection.api_credential() {
        command.env("LEMONADE_API_KEY", api_key);
    }
    if let Some(admin_api_key) = connection.admin_api_credential() {
        command.env("LEMONADE_ADMIN_API_KEY", admin_api_key);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            embedded.shutdown().await;
            return Err(error).with_context(|| format!("failed to start {program:?}"));
        }
    };

    let started = std::time::Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.tick().await;
    let exit_code = loop {
        tokio::select! {
            status = child.wait() => break status_to_code(status?),
            signal = shutdown_signal() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                break signal?;
            }
            _ = heartbeat.tick() => {
                eprintln!(
                    "test runtime: suite still running ({}s elapsed)",
                    started.elapsed().as_secs()
                );
            }
        }
    };

    embedded.shutdown().await;
    eprintln!("test runtime: embedded Lemonade and its backend processes stopped");
    Ok(exit_code)
}

fn status_to_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<i32> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to install SIGTERM handler")?;
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to wait for Ctrl-C")?;
            Ok(130)
        }
        _ = terminate.recv() => Ok(143),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<i32> {
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for Ctrl-C")?;
    Ok(130)
}
