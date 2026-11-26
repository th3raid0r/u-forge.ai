//! Lifecycle shared by chat producers, independent of either wire protocol.
use std::sync::Arc;

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use tokio::sync::mpsc;

use super::{AgentBudgetDiagnostics, ChatEvent, LemonadeRuntime, LemonadeRuntimeProfile};
use crate::queue::{CancellationToken, InferenceError};

#[derive(Debug, Clone)]
pub enum ChatRunError {
    ContextOverflow {
        reason: String,
        diagnostics: AgentBudgetDiagnostics,
    },
    RepeatStop {
        reason: String,
        diagnostics: AgentBudgetDiagnostics,
    },
    Interrupted(InferenceError),
    ReceiverClosed,
    Incomplete(&'static str),
    Runtime(String),
    Provider(String),
    Agent(String),
    DriverLost(String),
}

/// Success carries only the final turn's fallback text, never another delta.
pub type ChatRunOutcome = Result<Option<String>, ChatRunError>;
pub type ChatRunCompletion = Shared<BoxFuture<'static, ChatRunOutcome>>;

pub struct ChatRunContext {
    /// The shared manager for this run's selected device, not the picker default.
    pub gpu: Option<Arc<super::GpuResourceManager>>,
    pub profile: Option<(Arc<LemonadeRuntime>, LemonadeRuntimeProfile)>,
    pub cancellation: CancellationToken,
}

pub struct ChatRunHandle {
    pub events: mpsc::Receiver<ChatEvent>,
    pub completion: ChatRunCompletion,
    pub cancellation: CancellationToken,
}

pub struct ChatEventSink {
    tx: mpsc::Sender<ChatEvent>,
    cancellation: CancellationToken,
}

impl ChatEventSink {
    pub fn check_open(&self) -> Result<(), ChatRunError> {
        self.cancellation
            .check_cancelled()
            .map_err(ChatRunError::Interrupted)?;
        if self.tx.is_closed() {
            return Err(ChatRunError::ReceiverClosed);
        }
        Ok(())
    }

    pub async fn send(&self, event: ChatEvent) -> Result<(), ChatRunError> {
        self.check_open()?;
        self.tx
            .send(event)
            .await
            .map_err(|_| ChatRunError::ReceiverClosed)
    }
}

impl ChatRunContext {
    /// The outer select interrupts *all* waits, including a backpressured send,
    /// a tool future, and device (re)acquisition. Joining the driver witnesses
    /// destruction of its futures and guards, not just a protocol finish tag.
    pub fn start<F, Fut>(self, drive: F) -> ChatRunHandle
    where
        F: FnOnce(ChatEventSink, Option<Arc<super::GpuResourceManager>>) -> Fut + Send + 'static,
        Fut: Future<Output = ChatRunOutcome> + Send + 'static,
    {
        let (tx, events) = mpsc::channel(64);
        let cancellation = self.cancellation;
        let parent = cancellation.clone();
        let driver = tokio::spawn(async move {
            let operation = async {
                let _lease = match self.profile {
                    Some((runtime, profile)) => Some(
                        runtime
                            .acquire_with_cancellation(&profile, &parent)
                            .await
                            .map_err(|e| ChatRunError::Runtime(format!("{e:#}")))?,
                    ),
                    None => None,
                };
                drive(
                    ChatEventSink {
                        tx: tx.clone(),
                        cancellation: parent.clone(),
                    },
                    self.gpu,
                )
                .await
            };
            let outcome = tokio::select! {
                biased;
                _ = parent.cancelled() => Err(ChatRunError::Interrupted(parent.error())),
                _ = tx.closed() => Err(ChatRunError::ReceiverClosed),
                result = operation => result,
            };
            // A send or profile wait can itself notice interruption in the
            // same poll. Classify the cause here, not as a wire error.
            if parent.is_cancelled() {
                Err(ChatRunError::Interrupted(parent.error()))
            } else if tx.is_closed() {
                // Child queue jobs can outlive a dropped tool future.
                parent.cancel();
                Err(ChatRunError::ReceiverClosed)
            } else {
                outcome
            }
        });
        let completion = async move {
            driver
                .await
                .unwrap_or_else(|e| Err(ChatRunError::DriverLost(e.to_string())))
        }
        .boxed()
        .shared();
        ChatRunHandle {
            events,
            completion,
            cancellation,
        }
    }
}
