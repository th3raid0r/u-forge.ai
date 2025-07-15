//! embeddings.rs - Handles text embedding generation using various providers.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, ModelInfo, TextEmbedding, Embedding as FastEmbedEmbedding};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

#[cfg(test)]
use std::sync::OnceLock;

/// Defines the types of embedding providers available.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbeddingProviderType {
    Local(LocalEmbeddingModelType),
    Ollama, // Placeholder for future Ollama integration
    Cloud,  // Placeholder for future Cloud API integration
}

/// Specifies the type of local embedding model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LocalEmbeddingModelType {
    FastEmbed(FastEmbedModel),
}

/// Alias for specific FastEmbed models for easier configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FastEmbedModel {
    AllMiniLmL6V2,      // sentence-transformers/all-MiniLM-L6-v2
    BgeSmallEnV15,      // BAAI/bge-small-en-v1.5
    BgeBaseEnV15,       // BAAI/bge-base-en-v1.5
    BgeLargeEnV15,      // BAAI/bge-large-en-v1.5
    NomicEmbedTextV15,  // nomic-ai/nomic-embed-text-v1.5
    // Add more as needed
}

impl FastEmbedModel {
    pub fn to_embedding_model(&self) -> EmbeddingModel {
        match self {
            FastEmbedModel::AllMiniLmL6V2 => EmbeddingModel::AllMiniLML6V2,
            FastEmbedModel::BgeSmallEnV15 => EmbeddingModel::BGESmallENV15,
            FastEmbedModel::BgeBaseEnV15 => EmbeddingModel::BGEBaseENV15,
            FastEmbedModel::BgeLargeEnV15 => EmbeddingModel::BGELargeENV15,
            FastEmbedModel::NomicEmbedTextV15 => EmbeddingModel::NomicEmbedTextV15,
        }
    }

    pub fn default_model() -> Self {
        FastEmbedModel::NomicEmbedTextV15 // A good balance of size and performance
    }
}

/// Trait for embedding providers.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> Result<usize>;
    fn max_tokens(&self) -> Result<usize>; // Maximum tokens the model can handle in one go
    fn provider_type(&self) -> EmbeddingProviderType;
    fn model_info(&self) -> Option<ModelInfo<EmbeddingModel>>;
}

/// Embedding provider using FastEmbed for local, on-device embeddings.
pub struct FastEmbedProvider {
    model: Mutex<TextEmbedding>,
    model_info: ModelInfo<EmbeddingModel>,
    model_type: FastEmbedModel,
}

impl FastEmbedProvider {
    /// Creates a new FastEmbedProvider.
    ///
    /// # Arguments
    /// * `model_name` - The specific FastEmbed model to use.
    /// * `cache_dir` - Optional directory to cache downloaded models.
    /// * `show_download_progress` - Whether to display download progress.
    pub fn new(
        model_type: FastEmbedModel,
        cache_dir: Option<PathBuf>,
        show_download_progress: bool,
    ) -> Result<Self> {
        let embedding_model = model_type.to_embedding_model();
        let mut init_options = InitOptions::new(embedding_model.clone())
            .with_show_download_progress(show_download_progress);
        
        if let Some(cache_path) = cache_dir.clone() {
            init_options = init_options.with_cache_dir(cache_path);
        }

        info!(
            "Initializing FastEmbed model: {:?}, cache_dir: {:?}, show_progress: {}",
            embedding_model, cache_dir, show_download_progress
        );

        let model = TextEmbedding::try_new(init_options)
            .map_err(|e| anyhow!("Failed to initialize FastEmbed model: {}", e))?;

        let model_info = TextEmbedding::get_model_info(&embedding_model)?.clone();
        
        info!("FastEmbed model initialized: {:?}", model_info);

        Ok(Self { model: Mutex::new(model), model_info, model_type })
    }

    /// Creates a default FastEmbedProvider using `BgeSmallEnV15`.
    /// Recommended for most users.
    pub fn default(cache_dir: Option<PathBuf>, show_download_progress: bool) -> Result<Self> {
        Self::new(FastEmbedModel::default_model(), cache_dir, show_download_progress)
    }
}

#[async_trait]
impl EmbeddingProvider for FastEmbedProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // FastEmbed's embed function takes Vec<&str>, so we wrap the single string.
        // It returns Vec<Embedding>, so we take the first.
        let documents = vec![text];
        let embeddings: Vec<FastEmbedEmbedding> = self.model.lock()
            .map_err(|e| anyhow!("Failed to acquire model lock: {}", e))?
            .embed(documents, None)
            .map_err(|e| anyhow!("FastEmbed embedding failed: {}", e))?;
        
        embeddings.into_iter().next()
            .ok_or_else(|| anyhow!("FastEmbed returned no embedding for a single document"))
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        // Convert Vec<String> to Vec<&str> for FastEmbed
        let text_slices: Vec<&str> = texts.iter().map(AsRef::as_ref).collect();
        let embeddings: Vec<FastEmbedEmbedding> = self.model.lock()
            .map_err(|e| anyhow!("Failed to acquire model lock: {}", e))?
            .embed(text_slices, None)
            .map_err(|e| anyhow!("FastEmbed batch embedding failed: {}", e))?;
        Ok(embeddings)
    }

    fn dimensions(&self) -> Result<usize> {
        Ok(self.model_info.dim)
    }

    fn max_tokens(&self) -> Result<usize> {
        // FastEmbed models typically have a max sequence length.
        // Since ModelInfo doesn't expose max_length directly in the current version,
        // we'll use a reasonable default based on common embedding models.
        // BGE models, for example, often have 512.
        Ok(512)
    }

    fn provider_type(&self) -> EmbeddingProviderType {
        EmbeddingProviderType::Local(LocalEmbeddingModelType::FastEmbed(self.model_type.clone()))
    }

    fn model_info(&self) -> Option<ModelInfo<EmbeddingModel>> {
        Some(self.model_info.clone())
    }
}


/// Manages available embedding providers.
pub struct EmbeddingManager {
    provider: Arc<dyn EmbeddingProvider>,
}

impl EmbeddingManager {
    /// Creates a new EmbeddingManager, attempting to initialize the best available local provider.
    pub fn try_new_local_default(cache_dir: Option<PathBuf>) -> Result<Self> {
        // Attempt to initialize FastEmbedProvider as the default
        match FastEmbedProvider::default(cache_dir.clone(), true) {
            Ok(fastembed_provider) => {
                info!("Successfully initialized FastEmbedProvider as default.");
                Ok(Self {
                    provider: Arc::new(fastembed_provider),
                })
            }
            Err(e) => {
                // In a real app, you might try other providers or return a "NoOpProvider"
                // For now, we just error out if the default local can't be initialized.
                Err(anyhow!("Failed to initialize default local embedding provider (FastEmbed): {}", e))
            }
        }
    }
    
    /// Creates an EmbeddingManager with a specific FastEmbed model.
    pub fn new_fastembed(model_type: FastEmbedModel, cache_dir: Option<PathBuf>) -> Result<Self> {
        let provider = FastEmbedProvider::new(model_type, cache_dir, true)?;
        Ok(Self {
            provider: Arc::new(provider),
        })
    }

    pub fn get_provider(&self) -> Arc<dyn EmbeddingProvider> {
        self.provider.clone()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    // Global lock to prevent race conditions during model initialization
    static TEST_INIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    // Helper to ensure models are downloaded only once per test run for speed.
    // Note: For CI, you might pre-download models or use a shared cache.
    fn get_test_cache_dir() -> PathBuf {
        // Use environment variable for consistent caching across runs (set in env.sh)
        std::env::var("FASTEMBED_CACHE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("test_model_cache");
                std::fs::create_dir_all(&path).expect("Failed to create test model cache dir");
                // Set the environment variable for FastEmbed to use
                std::env::set_var("FASTEMBED_CACHE_PATH", &path);
                path
            })
    }

    // Helper to create providers with proper locking to avoid race conditions
    fn create_test_provider(model_type: FastEmbedModel) -> Result<FastEmbedProvider> {
        let _lock = TEST_INIT_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let cache_dir = get_test_cache_dir();
        FastEmbedProvider::new(model_type, Some(cache_dir), false)
    }

    fn create_test_embedding_manager() -> Result<EmbeddingManager> {
        let _lock = TEST_INIT_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let cache_dir = get_test_cache_dir();
        EmbeddingManager::try_new_local_default(Some(cache_dir))
    }

    #[tokio::test]
    async fn test_fastembed_provider_default_initialization() {
        let provider_result = create_test_provider(FastEmbedModel::default_model());
        assert!(provider_result.is_ok(), "Failed to initialize default FastEmbedProvider: {:?}", provider_result.err());
        
        if let Ok(provider) = provider_result {
            assert_eq!(provider.provider_type(), EmbeddingProviderType::Local(LocalEmbeddingModelType::FastEmbed(FastEmbedModel::default_model())));
            let dims = provider.dimensions().unwrap();
            // NomicEmbedTextV15 has 768 dimensions
            assert_eq!(dims, 768, "Default model (NomicEmbedTextV15) should have 768 dimensions");
            let model_info = provider.model_info().unwrap();
            assert_eq!(model_info.model, FastEmbedModel::default_model().to_embedding_model());
        }
    }
    
    #[tokio::test]
    async fn test_fastembed_provider_specific_model_initialization() {
        // Test with a different, very small model if available, e.g., AllMiniLML6V2
        let model_type = FastEmbedModel::AllMiniLmL6V2;
        let provider_result = create_test_provider(model_type.clone());
        assert!(provider_result.is_ok(), "Failed to initialize FastEmbedProvider with AllMiniLmL6V2: {:?}", provider_result.err());

        if let Ok(provider) = provider_result {
            assert_eq!(provider.provider_type(), EmbeddingProviderType::Local(LocalEmbeddingModelType::FastEmbed(model_type)));
            let dims = provider.dimensions().unwrap();
            // AllMiniLmL6V2 has 384 dimensions
            assert_eq!(dims, 384, "AllMiniLmL6V2 model should have 384 dimensions");
             let model_info = provider.model_info().unwrap();
            assert_eq!(model_info.model, FastEmbedModel::AllMiniLmL6V2.to_embedding_model());
        }
    }

    #[tokio::test]
    async fn test_fastembed_embed_single_document() {
        let provider = create_test_provider(FastEmbedModel::default_model()).unwrap();
        let text = "This is a test document.";
        let embedding_result = provider.embed(text).await;

        assert!(embedding_result.is_ok(), "Embedding failed: {:?}", embedding_result.err());
        let embedding = embedding_result.unwrap();
        assert_eq!(embedding.len(), provider.dimensions().unwrap(), "Embedding dimension mismatch");
        assert!(embedding.iter().all(|&x| x.is_finite()), "Embedding contains non-finite values");
    }

    #[tokio::test]
    async fn test_fastembed_embed_batch_documents() {
        let provider = create_test_provider(FastEmbedModel::default_model()).unwrap();
        let texts = vec![
            "First test document.".to_string(),
            "Second test document, slightly longer.".to_string(),
            "Third one.".to_string(),
        ];
        let embeddings_result = provider.embed_batch(texts.clone()).await;

        assert!(embeddings_result.is_ok(), "Batch embedding failed: {:?}", embeddings_result.err());
        let embeddings = embeddings_result.unwrap();
        assert_eq!(embeddings.len(), texts.len(), "Number of embeddings does not match number of texts");
        for embedding in embeddings {
            assert_eq!(embedding.len(), provider.dimensions().unwrap(), "Embedding dimension mismatch in batch");
            assert!(embedding.iter().all(|&x| x.is_finite()), "Batch embedding contains non-finite values");
        }
    }
    
    #[tokio::test]
    async fn test_embedding_manager_default_initialization() {
        let manager_result = create_test_embedding_manager();
        assert!(manager_result.is_ok(), "Failed to initialize EmbeddingManager: {:?}", manager_result.err());

        if let Ok(manager) = manager_result {
            let provider = manager.get_provider();
            assert_eq!(provider.provider_type(), EmbeddingProviderType::Local(LocalEmbeddingModelType::FastEmbed(FastEmbedModel::default_model())));
            assert!(provider.dimensions().unwrap() > 0);
        }
    }

    #[tokio::test]
    async fn test_embedding_manager_specific_fastembed_model() {
        let model_type = FastEmbedModel::AllMiniLmL6V2; // Use a known small model
        let _lock = TEST_INIT_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let cache_dir = get_test_cache_dir();
        let manager_result = EmbeddingManager::new_fastembed(model_type.clone(), Some(cache_dir));
        assert!(manager_result.is_ok(), "Failed to initialize EmbeddingManager with specific model: {:?}", manager_result.err());

        if let Ok(manager) = manager_result {
            let provider = manager.get_provider();
            assert_eq!(provider.provider_type(), EmbeddingProviderType::Local(LocalEmbeddingModelType::FastEmbed(model_type)));
            assert_eq!(provider.dimensions().unwrap(), 384);
        }
    }

}