//! embedding_queue.rs - Background embedding queue system for UI responsiveness
//!
//! This module provides a background worker system for handling embedding operations
//! without blocking the UI thread. It includes request queuing, progress tracking,
//! and cancellation support for optimal user experience in desktop applications.

use crate::embeddings::EmbeddingProvider;
use crate::types::{ChunkId, ObjectId};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task;
use uuid::Uuid;

/// Unique identifier for embedding requests
pub type RequestId = Uuid;

/// Status of an embedding request
#[derive(Debug, Clone, PartialEq)]
pub enum RequestStatus {
    Queued,
    Processing,
    Completed,
    Failed(String),
    Cancelled,
}

/// Progress information for embedding operations
#[derive(Debug, Clone)]
pub struct EmbeddingProgress {
    pub request_id: RequestId,
    pub status: RequestStatus,
    pub progress: Option<f32>, // 0.0 to 1.0 for batch operations
    pub message: String,
}

/// Request for single text embedding
#[derive(Debug)]
pub struct EmbeddingRequest {
    pub id: RequestId,
    pub text: String,
    pub chunk_id: Option<ChunkId>,
    pub object_id: Option<ObjectId>,
    pub response_sender: oneshot::Sender<Result<Vec<f32>>>,
}

/// Request for batch text embedding
#[derive(Debug)]
pub struct BatchEmbeddingRequest {
    pub id: RequestId,
    pub texts: Vec<String>,
    pub chunk_ids: Vec<Option<ChunkId>>,
    pub object_ids: Vec<Option<ObjectId>>,
    pub response_sender: oneshot::Sender<Result<Vec<Vec<f32>>>>,
}

/// Internal queue message types
#[derive(Debug)]
enum QueueMessage {
    Single(EmbeddingRequest),
    Batch(BatchEmbeddingRequest),
    Cancel(RequestId),
    Shutdown,
}

/// Background embedding queue with progress tracking
pub struct EmbeddingQueue {
    sender: mpsc::UnboundedSender<QueueMessage>,
    progress_receiver: Arc<RwLock<mpsc::UnboundedReceiver<EmbeddingProgress>>>,
    request_status: Arc<RwLock<HashMap<RequestId, RequestStatus>>>,
}

impl EmbeddingQueue {
    /// Create a new embedding queue with background worker
    pub fn new(embedding_provider: Arc<dyn EmbeddingProvider>) -> Self {
        let (queue_sender, mut queue_receiver) = mpsc::unbounded_channel();
        let (progress_sender, progress_receiver) = mpsc::unbounded_channel();
        let request_status = Arc::new(RwLock::new(HashMap::new()));
        let request_status_worker = request_status.clone();

        // Spawn background worker
        task::spawn(async move {
            Self::worker_loop(
                &mut queue_receiver,
                embedding_provider,
                progress_sender,
                request_status_worker,
            )
            .await;
        });

        Self {
            sender: queue_sender,
            progress_receiver: Arc::new(RwLock::new(progress_receiver)),
            request_status,
        }
    }

    /// Submit a single embedding request
    pub async fn embed_text(
        &self,
        text: String,
        chunk_id: Option<ChunkId>,
        object_id: Option<ObjectId>,
    ) -> Result<oneshot::Receiver<Result<Vec<f32>>>> {
        let request_id = Uuid::new_v4();
        let (response_sender, response_receiver) = oneshot::channel();

        let request = EmbeddingRequest {
            id: request_id,
            text,
            chunk_id,
            object_id,
            response_sender,
        };

        // Mark as queued
        {
            let mut status_map = self.request_status.write().await;
            status_map.insert(request_id, RequestStatus::Queued);
        }

        // Send to worker
        self.sender
            .send(QueueMessage::Single(request))
            .map_err(|_| anyhow::anyhow!("Queue worker has shut down"))?;

        Ok(response_receiver)
    }

    /// Submit a batch embedding request
    pub async fn embed_batch(
        &self,
        texts: Vec<String>,
        chunk_ids: Vec<Option<ChunkId>>,
        object_ids: Vec<Option<ObjectId>>,
    ) -> Result<oneshot::Receiver<Result<Vec<Vec<f32>>>>> {
        if texts.len() != chunk_ids.len() || texts.len() != object_ids.len() {
            return Err(anyhow::anyhow!(
                "texts, chunk_ids, and object_ids must have the same length"
            ));
        }

        let request_id = Uuid::new_v4();
        let (response_sender, response_receiver) = oneshot::channel();

        let request = BatchEmbeddingRequest {
            id: request_id,
            texts,
            chunk_ids,
            object_ids,
            response_sender,
        };

        // Mark as queued
        {
            let mut status_map = self.request_status.write().await;
            status_map.insert(request_id, RequestStatus::Queued);
        }

        // Send to worker
        self.sender
            .send(QueueMessage::Batch(request))
            .map_err(|_| anyhow::anyhow!("Queue worker has shut down"))?;

        Ok(response_receiver)
    }

    /// Cancel a pending request
    pub async fn cancel_request(&self, request_id: RequestId) -> Result<()> {
        self.sender
            .send(QueueMessage::Cancel(request_id))
            .map_err(|_| anyhow::anyhow!("Queue worker has shut down"))?;

        // Update status
        {
            let mut status_map = self.request_status.write().await;
            status_map.insert(request_id, RequestStatus::Cancelled);
        }

        Ok(())
    }

    /// Get the current status of a request
    pub async fn get_request_status(&self, request_id: RequestId) -> Option<RequestStatus> {
        let status_map = self.request_status.read().await;
        status_map.get(&request_id).cloned()
    }

    /// Get all pending request statuses
    pub async fn get_all_statuses(&self) -> HashMap<RequestId, RequestStatus> {
        let status_map = self.request_status.read().await;
        status_map.clone()
    }

    /// Receive progress updates (non-blocking)
    pub async fn try_recv_progress(&self) -> Option<EmbeddingProgress> {
        let mut receiver = self.progress_receiver.write().await;
        receiver.try_recv().ok()
    }

    /// Shutdown the queue worker
    pub async fn shutdown(&self) -> Result<()> {
        self.sender
            .send(QueueMessage::Shutdown)
            .map_err(|_| anyhow::anyhow!("Queue worker already shut down"))?;
        Ok(())
    }

    /// Background worker loop
    async fn worker_loop(
        receiver: &mut mpsc::UnboundedReceiver<QueueMessage>,
        provider: Arc<dyn EmbeddingProvider>,
        progress_sender: mpsc::UnboundedSender<EmbeddingProgress>,
        request_status: Arc<RwLock<HashMap<RequestId, RequestStatus>>>,
    ) {
        while let Some(message) = receiver.recv().await {
            match message {
                QueueMessage::Single(request) => {
                    Self::process_single_request(
                        request,
                        provider.clone(),
                        &progress_sender,
                        &request_status,
                    )
                    .await;
                }
                QueueMessage::Batch(request) => {
                    Self::process_batch_request(
                        request,
                        provider.clone(),
                        &progress_sender,
                        &request_status,
                    )
                    .await;
                }
                QueueMessage::Cancel(request_id) => {
                    // Note: Cancellation during processing is not supported by FastEmbed
                    // We can only cancel queued requests
                    let mut status_map = request_status.write().await;
                    if let Some(status) = status_map.get(&request_id) {
                        if matches!(status, RequestStatus::Queued) {
                            status_map.insert(request_id, RequestStatus::Cancelled);
                            let _ = progress_sender.send(EmbeddingProgress {
                                request_id,
                                status: RequestStatus::Cancelled,
                                progress: None,
                                message: "Request cancelled".to_string(),
                            });
                        }
                    }
                }
                QueueMessage::Shutdown => {
                    break;
                }
            }
        }
    }

    /// Process a single embedding request
    async fn process_single_request(
        request: EmbeddingRequest,
        provider: Arc<dyn EmbeddingProvider>,
        progress_sender: &mpsc::UnboundedSender<EmbeddingProgress>,
        request_status: &Arc<RwLock<HashMap<RequestId, RequestStatus>>>,
    ) {
        let request_id = request.id;

        // Check if cancelled
        {
            let status_map = request_status.read().await;
            if let Some(RequestStatus::Cancelled) = status_map.get(&request_id) {
                let _ = request.response_sender.send(Err(anyhow::anyhow!("Request cancelled")));
                return;
            }
        }

        // Update status to processing
        {
            let mut status_map = request_status.write().await;
            status_map.insert(request_id, RequestStatus::Processing);
        }

        let _ = progress_sender.send(EmbeddingProgress {
            request_id,
            status: RequestStatus::Processing,
            progress: None,
            message: "Generating embedding...".to_string(),
        });

        // Perform embedding in spawn_blocking to avoid blocking the async runtime
        let text = request.text;
        let provider_clone = provider.clone();
        
        let result = task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async {
                provider_clone.embed(&text).await
            })
        })
        .await;

        match result {
            Ok(Ok(embedding)) => {
                // Success
                {
                    let mut status_map = request_status.write().await;
                    status_map.insert(request_id, RequestStatus::Completed);
                }

                let _ = progress_sender.send(EmbeddingProgress {
                    request_id,
                    status: RequestStatus::Completed,
                    progress: Some(1.0),
                    message: "Embedding completed".to_string(),
                });

                let _ = request.response_sender.send(Ok(embedding));
            }
            Ok(Err(e)) => {
                // Embedding error
                let error_msg = format!("Embedding failed: {}", e);
                {
                    let mut status_map = request_status.write().await;
                    status_map.insert(request_id, RequestStatus::Failed(error_msg.clone()));
                }

                let _ = progress_sender.send(EmbeddingProgress {
                    request_id,
                    status: RequestStatus::Failed(error_msg.clone()),
                    progress: None,
                    message: error_msg.clone(),
                });

                let _ = request.response_sender.send(Err(e));
            }
            Err(e) => {
                // Task spawn error
                let error_msg = format!("Task execution failed: {}", e);
                {
                    let mut status_map = request_status.write().await;
                    status_map.insert(request_id, RequestStatus::Failed(error_msg.clone()));
                }

                let _ = progress_sender.send(EmbeddingProgress {
                    request_id,
                    status: RequestStatus::Failed(error_msg.clone()),
                    progress: None,
                    message: error_msg.clone(),
                });

                let _ = request.response_sender.send(Err(anyhow::anyhow!(e)));
            }
        }
    }

    /// Process a batch embedding request
    async fn process_batch_request(
        request: BatchEmbeddingRequest,
        provider: Arc<dyn EmbeddingProvider>,
        progress_sender: &mpsc::UnboundedSender<EmbeddingProgress>,
        request_status: &Arc<RwLock<HashMap<RequestId, RequestStatus>>>,
    ) {
        let request_id = request.id;
        let total_items = request.texts.len();

        // Check if cancelled
        {
            let status_map = request_status.read().await;
            if let Some(RequestStatus::Cancelled) = status_map.get(&request_id) {
                let _ = request.response_sender.send(Err(anyhow::anyhow!("Request cancelled")));
                return;
            }
        }

        // Update status to processing
        {
            let mut status_map = request_status.write().await;
            status_map.insert(request_id, RequestStatus::Processing);
        }

        let _ = progress_sender.send(EmbeddingProgress {
            request_id,
            status: RequestStatus::Processing,
            progress: Some(0.0),
            message: format!("Processing batch of {} items...", total_items),
        });

        // Process batch in spawn_blocking
        let texts = request.texts;
        let provider_clone = provider.clone();
        
        let result = task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async {
                provider_clone.embed_batch(texts).await
            })
        })
        .await;

        match result {
            Ok(Ok(embeddings)) => {
                // Success
                {
                    let mut status_map = request_status.write().await;
                    status_map.insert(request_id, RequestStatus::Completed);
                }

                let _ = progress_sender.send(EmbeddingProgress {
                    request_id,
                    status: RequestStatus::Completed,
                    progress: Some(1.0),
                    message: format!("Batch of {} items completed", embeddings.len()),
                });

                let _ = request.response_sender.send(Ok(embeddings));
            }
            Ok(Err(e)) => {
                // Embedding error
                let error_msg = format!("Batch embedding failed: {}", e);
                {
                    let mut status_map = request_status.write().await;
                    status_map.insert(request_id, RequestStatus::Failed(error_msg.clone()));
                }

                let _ = progress_sender.send(EmbeddingProgress {
                    request_id,
                    status: RequestStatus::Failed(error_msg.clone()),
                    progress: None,
                    message: error_msg.clone(),
                });

                let _ = request.response_sender.send(Err(e));
            }
            Err(e) => {
                // Task spawn error
                let error_msg = format!("Batch task execution failed: {}", e);
                {
                    let mut status_map = request_status.write().await;
                    status_map.insert(request_id, RequestStatus::Failed(error_msg.clone()));
                }

                let _ = progress_sender.send(EmbeddingProgress {
                    request_id,
                    status: RequestStatus::Failed(error_msg.clone()),
                    progress: None,
                    message: error_msg.clone(),
                });

                let _ = request.response_sender.send(Err(anyhow::anyhow!(e)));
            }
        }
    }
}

/// Builder for configuring embedding queue
pub struct EmbeddingQueueBuilder {
    max_queue_size: Option<usize>,
    max_concurrent_requests: Option<usize>,
}

impl EmbeddingQueueBuilder {
    pub fn new() -> Self {
        Self {
            max_queue_size: None,
            max_concurrent_requests: None,
        }
    }

    pub fn max_queue_size(mut self, size: usize) -> Self {
        self.max_queue_size = Some(size);
        self
    }

    pub fn max_concurrent_requests(mut self, count: usize) -> Self {
        self.max_concurrent_requests = Some(count);
        self
    }

    pub fn build(self, embedding_provider: Arc<dyn EmbeddingProvider>) -> EmbeddingQueue {
        // For now, use the simple implementation
        // Future versions could implement bounded queues and concurrency limits
        EmbeddingQueue::new(embedding_provider)
    }
}

impl Default for EmbeddingQueueBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingManager;
    use tempfile::TempDir;
    use tokio::time::{timeout, Duration};

    async fn create_test_queue() -> EmbeddingQueue {
        let temp_dir = TempDir::new().unwrap();
        let embedding_manager = EmbeddingManager::try_new_local_default(Some(temp_dir.path().to_path_buf()))
            .expect("Failed to create embedding manager");
        
        EmbeddingQueue::new(embedding_manager.get_provider())
    }

    #[tokio::test]
    async fn test_single_embedding_request() {
        let queue = create_test_queue().await;
        
        let receiver = queue
            .embed_text(
                "Test document".to_string(),
                None,
                None,
            )
            .await
            .expect("Failed to submit request");

        let result = timeout(Duration::from_secs(10), receiver)
            .await
            .expect("Request timed out")
            .expect("Failed to receive response")
            .expect("Embedding failed");

        assert_eq!(result.len(), 384); // BGE-small-en-v1.5 dimensions
    }

    #[tokio::test]
    async fn test_batch_embedding_request() {
        let queue = create_test_queue().await;
        
        let texts = vec![
            "First document".to_string(),
            "Second document".to_string(),
            "Third document".to_string(),
        ];
        
        let receiver = queue
            .embed_batch(
                texts.clone(),
                vec![None; texts.len()],
                vec![None; texts.len()],
            )
            .await
            .expect("Failed to submit batch request");

        let result = timeout(Duration::from_secs(15), receiver)
            .await
            .expect("Batch request timed out")
            .expect("Failed to receive batch response")
            .expect("Batch embedding failed");

        assert_eq!(result.len(), texts.len());
        for embedding in result {
            assert_eq!(embedding.len(), 384);
        }
    }

    #[tokio::test]
    async fn test_request_status_tracking() {
        let queue = create_test_queue().await;
        
        let receiver = queue
            .embed_text(
                "Status test document".to_string(),
                None,
                None,
            )
            .await
            .expect("Failed to submit request");

        // Give it a moment to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check that we have some status updates
        let all_statuses = queue.get_all_statuses().await;
        assert!(!all_statuses.is_empty());

        // Wait for completion
        let _result = timeout(Duration::from_secs(10), receiver)
            .await
            .expect("Request timed out");
    }

    #[tokio::test]
    async fn test_progress_updates() {
        let queue = create_test_queue().await;
        
        let _receiver = queue
            .embed_text(
                "Progress test document".to_string(),
                None,
                None,
            )
            .await
            .expect("Failed to submit request");

        // Check for progress updates
        let mut received_progress = false;
        for _ in 0..10 {
            if queue.try_recv_progress().await.is_some() {
                received_progress = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert!(received_progress, "Should have received progress updates");
    }
}