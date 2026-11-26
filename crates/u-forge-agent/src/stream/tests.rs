use super::*;
use futures::stream;
use rig::{
    OneOrMany,
    completion::{
        AssistantContent,
        message::{ToolCall, ToolFunction, ToolResult},
    },
};
use u_forge_core::{
    lemonade::GpuWorkload,
    queue::{CancellationToken, InferenceError},
};

type Item = MultiTurnStreamItem<()>;

async fn run_request(url: &str, params: AgentParams) -> ChatRunOutcome {
    let temp = tempfile::tempdir().unwrap();
    let graph = Arc::new(u_forge_core::KnowledgeGraph::new(temp.path()).unwrap());
    let queue = Arc::new(u_forge_core::queue::InferenceQueueBuilder::new().build());
    let agent = GraphAgent::new(url, graph, queue, None, "system").unwrap();
    let mut run = Arc::new(agent).start_run(AgentRunRequest {
        model_id: "model".into(),
        prompt: "search".into(),
        history: vec![],
        reasoning: ReasoningPolicy::Default,
        params,
        context: context(),
    });
    while run.events.recv().await.is_some() {}
    run.completion.await
}

#[tokio::test]
async fn irreducible_context_is_not_a_provider_failure() {
    let mut params = AgentParams::default();
    params.budget.context_tokens = 1;
    let result = run_request("http://127.0.0.1:1/v1", params).await;
    assert!(
        matches!(result, Err(ChatRunError::ContextOverflow { diagnostics, .. }) if diagnostics.model_calls == 0)
    );
}

#[tokio::test]
async fn unchanged_tool_call_ends_with_repeat_not_rig_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            assert!(socket.read(&mut bytes).await.unwrap() > 0);
            let body = concat!(
                "data: {\"id\":\"chat\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"s\",\"type\":\"function\",\"function\":{\"name\":\"search_fts\",\"arguments\":\"{\\\"query\\\":\\\"lore\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chat\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            );
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    let mut params = AgentParams::default();
    params.budget.repeated_call_limit = 0;
    let outcome =
        tokio::time::timeout(std::time::Duration::from_secs(5), run_request(&url, params))
            .await
            .unwrap();
    assert!(
        matches!(outcome, Err(ChatRunError::RepeatStop { .. })),
        "{outcome:?}"
    );
    server.await.unwrap();
}

#[test]
fn pending_repeat_observation_does_not_mask_completion_error() {
    let error = rig::completion::CompletionError::ResponseError("upstream unavailable".into());
    let result = agent_error(
        error.into(),
        Some(BudgetTermination::Repeat("repeat".into())),
        Default::default(),
    );
    assert!(
        matches!(result, ChatRunError::Provider(message) if message.contains("upstream unavailable"))
    );
}

fn context() -> ChatRunContext {
    ChatRunContext {
        profile: None,
        gpu: None,
        cancellation: CancellationToken::new(),
    }
}

fn run(stream: StreamingResult<()>, gpu: Arc<GpuResourceManager>) -> ChatRunHandle {
    context().start(move |sink, _| async move {
        let budget = BudgetController::new(
            Default::default(),
            None,
            "system".into(),
            String::new(),
            String::new(),
            String::new(),
            crate::estimate_tokens(""),
        );
        let guard = acquire_gpu(Some(&gpu)).await;
        drive_turns(stream, &sink, &budget, Some(&gpu), guard).await
    })
}

fn call(id: &str) -> Item {
    Item::StreamAssistantItem(StreamedAssistantContent::ToolCall {
        tool_call: ToolCall::new(
            id.into(),
            ToolFunction::new("upsert_node".into(), serde_json::json!({})),
        ),
        internal_call_id: id.into(),
    })
}

fn result(id: &str) -> Item {
    Item::StreamUserItem(StreamedUserContent::tool_result(
        ToolResult {
            id: id.into(),
            call_id: None,
            content: OneOrMany::one(ToolResultContent::text("ok")),
        },
        id.into(),
    ))
}

#[tokio::test]
async fn batch_releases_gpu_until_last_result_and_preserves_chronology() {
    let gpu = GpuResourceManager::new();
    let items = [
        Item::StreamAssistantItem(StreamedAssistantContent::ReasoningDelta {
            id: None,
            reasoning: "plan".into(),
        }),
        Item::StreamAssistantItem(StreamedAssistantContent::text("before")),
        call("1"),
        call("2"),
        result("1"),
        result("2"),
        Item::final_response(
            OneOrMany::one(AssistantContent::text("fallback")),
            Default::default(),
        ),
    ];
    let device = gpu.clone();
    let stream = stream::iter(items.into_iter().enumerate().map(move |(i, item)| {
        assert_eq!(
            device.current_workload(),
            if (3..=5).contains(&i) {
                GpuWorkload::Idle
            } else {
                GpuWorkload::LlmActive
            }
        );
        Ok(item)
    }));
    let mut handle = run(Box::pin(stream), gpu.clone());
    let mut events = vec![];
    while let Some(event) = handle.events.recv().await {
        events.push(event);
    }
    assert!(
        matches!(events.as_slice(), [ChatEvent::ReasoningDelta(r), ChatEvent::TextDelta(t), ChatEvent::ToolCallStart {internal_id: a,..}, ChatEvent::ToolCallStart {internal_id:b,..}, ChatEvent::ToolResult {internal_id:c,..}, ChatEvent::ToolResult {internal_id:d,..}, ChatEvent::AgentDiagnostics(_)] if r == "plan" && t == "before" && a == c && b == d && a != b)
    );
    assert_eq!(
        handle.completion.await.unwrap().as_deref(),
        Some("fallback")
    );
    assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
}

#[tokio::test]
async fn supersession_interrupts_reacquisition_without_polling_next_turn() {
    let gpu = GpuResourceManager::new();
    let device = gpu.clone();
    let (hold, held) = tokio::sync::oneshot::channel();
    let tools = stream::once(async { Ok(call("1")) })
        .chain(stream::once(async move {
            hold.send(device.begin_stt().unwrap()).unwrap();
            Ok(result("1"))
        }))
        .chain(stream::once(async {
            panic!("next model turn polled during STT")
        }));
    let mut handle = run(Box::pin(tools), gpu.clone());
    let stt = held.await.unwrap();
    assert!(matches!(
        handle.events.recv().await,
        Some(ChatEvent::ToolCallStart { .. })
    ));
    assert!(matches!(
        handle.events.recv().await,
        Some(ChatEvent::ToolResult { .. })
    ));
    handle.cancellation.supersede();
    assert!(matches!(
        handle.completion.await,
        Err(ChatRunError::Interrupted(InferenceError::Superseded))
    ));
    drop(stt);
    assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
}

#[tokio::test]
async fn receiver_drop_interrupts_pending_tool_and_cancels_child_token() {
    let gpu = GpuResourceManager::new();
    let (resume, waiting) = tokio::sync::oneshot::channel::<()>();
    let stream = stream::once(async { Ok(call("write")) }).chain(stream::once(async {
        let _ = waiting.await;
        panic!("unobserved tool committed")
    }));
    let mut handle = run(Box::pin(stream), gpu.clone());
    assert!(matches!(
        handle.events.recv().await,
        Some(ChatEvent::ToolCallStart { .. })
    ));
    drop(handle.events);
    let _ = resume.send(());
    assert!(matches!(
        handle.completion.await,
        Err(ChatRunError::ReceiverClosed)
    ));
    assert!(handle.cancellation.is_cancelled());
    assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
}
