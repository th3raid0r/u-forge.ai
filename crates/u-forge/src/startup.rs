//! Startup timeline and the synchronous application bootstrap shared by the
//! desktop binary and startup regression tests.

use std::{
    collections::HashSet,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use serde::Serialize;
use u_forge_core::{AppConfig, KnowledgeGraph, SchemaManager};
use u_forge_graph_view::{GraphSnapshot, build_snapshot};

/// Existing user-visible discovery message. Keeping it as a constant makes the
/// log boundary and the startup test assert the same contract.
pub const LEMONADE_METADATA_READY_MESSAGE: &str = "Lemonade connected — capabilities discovered";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupScenario {
    Normal,
    Fresh,
    Configured,
}

impl StartupScenario {
    fn from_env() -> Self {
        match std::env::var("UFORGE_STARTUP_PROFILE").as_deref() {
            Ok("fresh") => Self::Fresh,
            Ok("configured") => Self::Configured,
            _ => Self::Normal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Fresh => "fresh",
            Self::Configured => "configured",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupMilestone {
    AppFirstPaint,
    LemonadeMetadataReady,
    SetupFirstPaint,
    StartupReady,
}

impl StartupMilestone {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppFirstPaint => "app_first_paint",
            Self::LemonadeMetadataReady => "lemonade_metadata_ready",
            Self::SetupFirstPaint => "setup_first_paint",
            Self::StartupReady => "startup_ready",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StartupEvent {
    pub scenario: StartupScenario,
    pub kind: &'static str,
    pub phase: String,
    pub elapsed_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
}

struct StartupTimelineInner {
    events: Vec<StartupEvent>,
    milestones: HashSet<StartupMilestone>,
    output: Option<BufWriter<File>>,
}

/// Cloneable launch clock used across the synchronous main-thread bootstrap,
/// GPUI paint callbacks, and detached Lemonade initialization tasks.
#[derive(Clone)]
pub struct StartupTimeline {
    started: Instant,
    scenario: StartupScenario,
    exit_after: Option<StartupMilestone>,
    inner: Arc<Mutex<StartupTimelineInner>>,
}

impl Default for StartupTimeline {
    fn default() -> Self {
        Self::new(StartupScenario::Normal)
    }
}

impl StartupTimeline {
    pub fn new(scenario: StartupScenario) -> Self {
        Self {
            started: Instant::now(),
            scenario,
            exit_after: None,
            inner: Arc::new(Mutex::new(StartupTimelineInner {
                events: Vec::new(),
                milestones: HashSet::new(),
                output: None,
            })),
        }
    }

    /// Construct the production timeline. Profile runs write JSONL eagerly so
    /// a partial report survives a hang or forced termination.
    pub fn from_env() -> Self {
        let scenario = StartupScenario::from_env();
        let mut timeline = Self::new(scenario);
        timeline.exit_after = match std::env::var("UFORGE_STARTUP_EXIT_AFTER").as_deref() {
            Ok("app_first_paint") => Some(StartupMilestone::AppFirstPaint),
            Ok("lemonade_metadata_ready") => Some(StartupMilestone::LemonadeMetadataReady),
            Ok("setup_first_paint") => Some(StartupMilestone::SetupFirstPaint),
            Ok("startup_ready") => Some(StartupMilestone::StartupReady),
            _ => None,
        };

        if scenario != StartupScenario::Normal {
            let output = std::env::var_os("UFORGE_STARTUP_PROFILE_OUTPUT")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from("target/startup-profiles").join(format!(
                        "{}-{}.jsonl",
                        scenario.as_str(),
                        std::process::id()
                    ))
                });
            match open_output(&output) {
                Ok(writer) => {
                    timeline.inner.lock().output = Some(writer);
                    eprintln!("Startup profile: {}", output.display());
                }
                Err(error) => eprintln!(
                    "Warning: could not create startup profile {}: {error}",
                    output.display()
                ),
            }
        }
        timeline
    }

    pub fn scenario(&self) -> StartupScenario {
        self.scenario
    }

    pub fn phase(&self, phase: impl Into<String>) -> StartupPhase {
        StartupPhase {
            timeline: self.clone(),
            phase: phase.into(),
            started: Instant::now(),
        }
    }

    pub fn milestone(&self, milestone: StartupMilestone) -> bool {
        let mut inner = self.inner.lock();
        if !inner.milestones.insert(milestone) {
            return false;
        }
        let event = StartupEvent {
            scenario: self.scenario,
            kind: "milestone",
            phase: milestone.as_str().to_string(),
            elapsed_us: self.started.elapsed().as_micros() as u64,
            duration_us: None,
        };
        tracing::info!(
            startup_scenario = self.scenario.as_str(),
            startup_phase = milestone.as_str(),
            elapsed_us = event.elapsed_us,
            "Startup milestone"
        );
        push_event(&mut inner, event);
        true
    }

    pub fn contains(&self, milestone: StartupMilestone) -> bool {
        self.inner.lock().milestones.contains(&milestone)
    }

    pub fn should_exit_after(&self, milestone: StartupMilestone) -> bool {
        self.exit_after == Some(milestone)
    }

    pub fn events(&self) -> Vec<StartupEvent> {
        self.inner.lock().events.clone()
    }

    fn finish_phase(&self, phase: String, started: Instant) {
        let event = StartupEvent {
            scenario: self.scenario,
            kind: "phase",
            phase,
            elapsed_us: self.started.elapsed().as_micros() as u64,
            duration_us: Some(started.elapsed().as_micros() as u64),
        };
        tracing::info!(
            startup_scenario = self.scenario.as_str(),
            startup_phase = event.phase,
            elapsed_us = event.elapsed_us,
            duration_us = event.duration_us.unwrap_or_default(),
            "Startup phase completed"
        );
        push_event(&mut self.inner.lock(), event);
    }
}

pub struct StartupPhase {
    timeline: StartupTimeline,
    phase: String,
    started: Instant,
}

impl Drop for StartupPhase {
    fn drop(&mut self) {
        self.timeline
            .finish_phase(std::mem::take(&mut self.phase), self.started);
    }
}

fn open_output(path: &Path) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating startup profile directory {}", parent.display()))?;
    }
    let file = File::create(path)
        .with_context(|| format!("creating startup profile {}", path.display()))?;
    Ok(BufWriter::new(file))
}

fn push_event(inner: &mut StartupTimelineInner, event: StartupEvent) {
    if let Some(output) = &mut inner.output
        && serde_json::to_writer(&mut *output, &event).is_ok()
    {
        let _ = output.write_all(b"\n");
        let _ = output.flush();
    }
    inner.events.push(event);
}

/// Data prepared before GPUI starts. Keeping this in the library lets the
/// deterministic startup tests exercise the exact production bootstrap.
pub struct PreparedApp {
    pub snapshot: GraphSnapshot,
    pub graph: Arc<KnowledgeGraph>,
    pub schema_manager: Arc<SchemaManager>,
}

/// Locate the read-only defaults tree staged beside Cargo binaries or in the
/// standard AppDir share directory used by the AppImage.
pub fn packaged_defaults_dir() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("locating the u-forge executable")?;
    packaged_defaults_dir_for(&executable)
}

fn packaged_defaults_dir_for(executable: &Path) -> Result<PathBuf> {
    let executable_dir = executable
        .parent()
        .context("u-forge executable has no parent directory")?;
    let profile_dir = match executable_dir.file_name().and_then(|name| name.to_str()) {
        Some("deps" | "examples") => executable_dir
            .parent()
            .context("Cargo executable directory has no profile parent")?,
        _ => executable_dir,
    };
    [
        profile_dir.join("defaults"),
        executable_dir.join("../share/u-forge/defaults"),
    ]
    .into_iter()
    .find(|path| path.join("config/u-forge.toml").is_file())
    .ok_or_else(|| {
        anyhow::anyhow!(
            "u-forge defaults were not found beside {} or in its AppDir share directory",
            executable.display()
        )
    })
}

pub fn prepare_app(
    config: &Arc<AppConfig>,
    runtime: &Arc<tokio::runtime::Runtime>,
    timeline: &StartupTimeline,
) -> Result<PreparedApp> {
    let _bootstrap = timeline.phase("local_bootstrap");
    runtime.block_on(async {
        let graph = {
            let _phase = timeline.phase("knowledge_graph_open");
            Arc::new(KnowledgeGraph::with_storage_config(&config.storage)?)
        };

        let schema_manager = graph.get_schema_manager();
        let schema_names = {
            let _phase = timeline.phase("schema_names_read");
            schema_manager.list_schemas().unwrap_or_default()
        };
        let has_real_schemas = schema_names.iter().any(|name| name != "default");
        if schema_names.is_empty() {
            let _phase = timeline.phase("default_schema_bootstrap");
            if let Err(error) = schema_manager.load_schema("default").await {
                eprintln!("Warning: could not create default schema: {error}");
            }
        } else {
            let _phase = timeline.phase("schema_cache_hydration");
            for name in &schema_names {
                if name == "default" && has_real_schemas {
                    let _ = schema_manager.delete_schema("default");
                    continue;
                }
                if let Err(error) = schema_manager.load_schema(name).await {
                    eprintln!("Warning: could not load schema '{name}': {error}");
                }
            }
        }

        let snapshot = {
            let _phase = timeline.phase("graph_snapshot_build");
            build_snapshot(&graph)?
        };
        tracing::info!(
            nodes = snapshot.nodes.len(),
            edges = snapshot.edges.len(),
            "Initial graph snapshot built"
        );
        Ok(PreparedApp {
            snapshot,
            graph,
            schema_manager,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_template(defaults: &Path) {
        let config = defaults.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("u-forge.toml"), "[storage]\n").unwrap();
    }

    #[test]
    fn packaged_defaults_resolve_beside_cargo_profile() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("target/debug");
        create_template(&profile.join("defaults"));

        assert_eq!(
            packaged_defaults_dir_for(&profile.join("u-forge")).unwrap(),
            profile.join("defaults")
        );
        assert_eq!(
            packaged_defaults_dir_for(&profile.join("deps/u_forge-test")).unwrap(),
            profile.join("defaults")
        );
    }

    #[test]
    fn packaged_defaults_resolve_from_appdir_share() {
        let temp = tempfile::tempdir().unwrap();
        let appdir = temp.path().join("u-forge.AppDir/usr");
        let defaults = appdir.join("share/u-forge/defaults");
        create_template(&defaults);
        std::fs::create_dir_all(appdir.join("bin")).unwrap();

        assert_eq!(
            packaged_defaults_dir_for(&appdir.join("bin/u-forge")).unwrap(),
            appdir.join("bin/../share/u-forge/defaults")
        );
    }
}
