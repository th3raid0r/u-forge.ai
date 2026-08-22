use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use gpui::TestAppContext;
use parking_lot::Mutex;
use tempfile::TempDir;
use u_forge_core::{AppConfig, KnowledgeGraph, lemonade::LemonadeConnection};

use crate::{
    AppView, UiTheme,
    chat_history::ChatHistoryStore,
    startup::{
        LEMONADE_METADATA_READY_MESSAGE, StartupMilestone, StartupScenario, StartupTimeline,
        prepare_app,
    },
};

struct FakeLemonade {
    base_url: String,
    stopping: Arc<AtomicBool>,
    max_in_flight: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeLemonade {
    fn fresh() -> Self {
        Self::start(false)
    }

    fn configured() -> Self {
        Self::start(true)
    }

    fn start(configured: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_stopping = stopping.clone();
        let server_max = max_in_flight.clone();
        let server_requests = requests.clone();
        let thread = thread::spawn(move || {
            let in_flight = Arc::new(AtomicUsize::new(0));
            while !server_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let in_flight = in_flight.clone();
                        let max_in_flight = server_max.clone();
                        let requests = server_requests.clone();
                        thread::spawn(move || {
                            serve_request(
                                stream,
                                configured,
                                &in_flight,
                                &max_in_flight,
                                &requests,
                            );
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            stopping,
            max_in_flight,
            requests,
            thread: Some(thread),
        }
    }

    fn connection(&self) -> Arc<LemonadeConnection> {
        Arc::new(LemonadeConnection::external(&self.base_url).unwrap())
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::Acquire)
    }

    fn request_count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .iter()
            .filter(|request| request.as_str() == path)
            .count()
    }
}

impl Drop for FakeLemonade {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let address = self.base_url.trim_start_matches("http://");
        let address = address.trim_end_matches("/v1");
        let _ = TcpStream::connect(address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_request(
    mut stream: TcpStream,
    configured: bool,
    in_flight: &AtomicUsize,
    max_in_flight: &AtomicUsize,
    requests: &Mutex<Vec<String>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stream.read(&mut buffer).unwrap_or(0);
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&bytes);
    let mut parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default().to_string();
    requests.lock().push(path.clone());

    let active = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
    max_in_flight.fetch_max(active, Ordering::AcqRel);
    thread::sleep(Duration::from_millis(15));
    let body = response_body(configured, method, &path);
    in_flight.fetch_sub(1, Ordering::AcqRel);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn response_body(configured: bool, method: &str, path: &str) -> String {
    if method == "POST" && path.ends_with("/embeddings") {
        return serde_json::json!({
            "object": "list",
            "data": [{
                "object": "embedding",
                "embedding": vec![0.0_f32; 768],
                "index": 0
            }],
            "model": "test-embedding",
            "usage": {"prompt_tokens": 1, "total_tokens": 1}
        })
        .to_string();
    }
    if path.ends_with("/downloads") {
        return serde_json::json!({"downloads": []}).to_string();
    }
    if path.contains("/models") {
        let models = if configured {
            vec![
                model(
                    "ggml-org/embeddinggemma-300M-GGUF",
                    "llamacpp",
                    &["embeddings"],
                    "ggml-org/embeddinggemma-300M-GGUF:Q8_0",
                ),
                model(
                    "bge-reranker-v2-m3-GGUF",
                    "llamacpp",
                    &["reranking"],
                    "bge-reranker-v2-m3-GGUF",
                ),
                model(
                    "Gemma-4-E4B-it-GGUF",
                    "llamacpp",
                    &["chat", "tool-calling"],
                    "Gemma-4-E4B-it-GGUF",
                ),
            ]
        } else {
            Vec::new()
        };
        return serde_json::json!({"data": models}).to_string();
    }
    if path.ends_with("/system-info") {
        let recipes = if configured {
            serde_json::json!({
                "llamacpp": {
                    "backends": {
                        "cpu": {
                            "state": "installed",
                            "devices": ["cpu"],
                            "version": "test"
                        }
                    }
                }
            })
        } else {
            serde_json::json!({})
        };
        return serde_json::json!({
            "Processor": "test cpu",
            "Physical Memory": "16 GB",
            "recipes": recipes
        })
        .to_string();
    }
    if path.ends_with("/health") {
        let loaded = if configured {
            [
                "ggml-org/embeddinggemma-300M-GGUF",
                "bge-reranker-v2-m3-GGUF",
                "Gemma-4-E4B-it-GGUF",
            ]
            .into_iter()
            .map(|name| {
                serde_json::json!({
                    "model_name": name,
                    "recipe": "llamacpp",
                    "device": "cpu",
                    "type": if name.contains("embedding") { "embedding" } else { "llm" }
                })
            })
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        return serde_json::json!({
            "version": "test",
            "status": "ok",
            "all_models_loaded": loaded,
            "max_models": {"embedding": 2}
        })
        .to_string();
    }
    serde_json::json!({}).to_string()
}

fn model(id: &str, recipe: &str, labels: &[&str], checkpoint: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "recipe": recipe,
        "labels": labels,
        "downloaded": true,
        "checkpoint": checkpoint,
        "recipe_options": {"llamacpp_backend": "cpu"}
    })
}

fn load_test_config(root: &Path, configured: bool, timeline: &StartupTimeline) -> Arc<AppConfig> {
    let source_path = root.join("config.toml");
    if configured && !source_path.exists() {
        std::fs::write(
            &source_path,
            format!(
                "[storage]\ndb_path = {:?}\n\n[embedding]\nnpu_enabled = false\nhigh_quality_embedding = false\n",
                root.join("db")
            ),
        )
        .unwrap();
    }
    let _phase = timeline.phase("config_load");
    let mut config = AppConfig::load(&source_path).unwrap();
    // A missing config intentionally resolves built-in defaults. Redirect only
    // its database path so the fresh-start test remains isolated.
    config.storage.db_path = root.join("db");
    config.embedding.npu_enabled = false;
    config.embedding.high_quality_embedding = false;
    Arc::new(config)
}

fn event_index(timeline: &StartupTimeline, phase: &str) -> usize {
    timeline
        .events()
        .iter()
        .position(|event| event.phase == phase)
        .unwrap_or_else(|| panic!("startup event {phase:?} was not recorded"))
}

#[gpui::test]
fn fresh_start_measures_until_setup_is_painted(cx: &mut TestAppContext) {
    cx.update(UiTheme::init);
    let temp = TempDir::new().unwrap();
    let server = FakeLemonade::fresh();
    let timeline = StartupTimeline::new(StartupScenario::Fresh);
    let config = load_test_config(temp.path(), false, &timeline);
    let runtime = {
        let _phase = timeline.phase("tokio_runtime_create");
        Arc::new(tokio::runtime::Runtime::new().unwrap())
    };
    let prepared = prepare_app(&config, &runtime, &timeline).unwrap();
    let data_file = config.data.import_file.clone();
    let schema_dir = config.data.schema_dir.clone();
    let connection = server.connection();
    let test_timeline = timeline.clone();

    let (view, cx) = cx.add_window_view(move |_, cx| {
        AppView::new_profiled(
            prepared.snapshot,
            prepared.graph,
            prepared.schema_manager,
            data_file,
            schema_dir,
            config,
            runtime,
            test_timeline,
            Some(connection),
            cx,
        )
    });
    cx.run_until_parked();

    assert!(cx.read(|app| view.read(app).setup_open));
    assert!(timeline.contains(StartupMilestone::SetupFirstPaint));
    assert!(
        event_index(&timeline, StartupMilestone::LemonadeMetadataReady.as_str())
            < event_index(&timeline, StartupMilestone::SetupFirstPaint.as_str())
    );
    assert_eq!(server.request_count("/v1/models?show_all=true"), 1);
    assert_eq!(server.request_count("/v1/downloads"), 1);
    assert!(server.max_in_flight() >= 2, "metadata GETs must overlap");
}

#[gpui::test]
fn configured_start_measures_metadata_before_activation(cx: &mut TestAppContext) {
    cx.update(UiTheme::init);
    let temp = TempDir::new().unwrap();
    let server = FakeLemonade::configured();

    // Pre-existing config, graph DB, schema cache, and chat DB are fixture setup,
    // deliberately outside the measured interval.
    let preseed_timeline = StartupTimeline::new(StartupScenario::Normal);
    let preseed_config = load_test_config(temp.path(), true, &preseed_timeline);
    let preseed_runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    drop(prepare_app(&preseed_config, &preseed_runtime, &preseed_timeline).unwrap());
    drop(KnowledgeGraph::with_storage_config(&preseed_config.storage).unwrap());
    drop(ChatHistoryStore::open(&preseed_config.storage.db_path).unwrap());

    let timeline = StartupTimeline::new(StartupScenario::Configured);
    let config = load_test_config(temp.path(), true, &timeline);
    let runtime = {
        let _phase = timeline.phase("tokio_runtime_create");
        Arc::new(tokio::runtime::Runtime::new().unwrap())
    };
    let prepared = prepare_app(&config, &runtime, &timeline).unwrap();
    let data_file = config.data.import_file.clone();
    let schema_dir = config.data.schema_dir.clone();
    let connection = server.connection();
    let test_timeline = timeline.clone();
    let (view, cx) = cx.add_window_view(move |_, cx| {
        AppView::new_profiled(
            prepared.snapshot,
            prepared.graph,
            prepared.schema_manager,
            data_file,
            schema_dir,
            config,
            runtime,
            test_timeline,
            Some(connection),
            cx,
        )
    });
    cx.run_until_parked();

    assert_eq!(
        LEMONADE_METADATA_READY_MESSAGE,
        "Lemonade connected — capabilities discovered"
    );
    assert!(timeline.contains(StartupMilestone::LemonadeMetadataReady));
    assert!(!cx.read(|app| view.read(app).setup_open));
    assert!(
        event_index(&timeline, StartupMilestone::LemonadeMetadataReady.as_str())
            < event_index(&timeline, "standard_inference_queue_build")
    );
    assert!(timeline.contains(StartupMilestone::StartupReady));
    assert_eq!(server.request_count("/v1/models?show_all=true"), 1);
    assert_eq!(server.request_count("/v1/downloads"), 1);
    assert!(server.max_in_flight() >= 2, "metadata GETs must overlap");
}

#[test]
fn startup_events_are_unique_and_keep_phase_durations() {
    let timeline = StartupTimeline::new(StartupScenario::Fresh);
    {
        let _phase = timeline.phase("test_phase");
    }
    assert!(timeline.milestone(StartupMilestone::AppFirstPaint));
    assert!(!timeline.milestone(StartupMilestone::AppFirstPaint));
    let counts =
        timeline
            .events()
            .into_iter()
            .fold(HashMap::<String, usize>::new(), |mut counts, event| {
                *counts.entry(event.phase).or_default() += 1;
                counts
            });
    assert_eq!(counts.get("test_phase"), Some(&1));
    assert_eq!(counts.get("app_first_paint"), Some(&1));
}
