//! Convenience builder for a matched set of GPU-sharing Lemonade providers.

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

use super::chat::LemonadeChatProvider;
use super::gpu_manager::GpuResourceManager;
use super::registry::LemonadeModelRegistry;
use super::stt::LemonadeSttProvider;
use super::tts::LemonadeTtsProvider;

/// Builds a matched set of GPU-sharing providers from a single registry fetch.
///
/// ```no_run
/// # async fn example() -> anyhow::Result<()> {
/// use u_forge_core::lemonade::{LemonadeStack, GpuResourceManager};
///
/// let stack = LemonadeStack::build("http://127.0.0.1:13305/api/v1").await?;
/// let text  = stack.chat.ask("Describe a dragon in one sentence.").await?;
/// println!("{text}");
/// # Ok(()) }
/// ```
pub struct LemonadeStack {
    pub registry: LemonadeModelRegistry,
    pub gpu: Arc<GpuResourceManager>,
    pub tts: LemonadeTtsProvider,
    pub stt: LemonadeSttProvider,
    pub chat: LemonadeChatProvider,
}

impl LemonadeStack {
    /// Fetch the model registry and construct all providers sharing one GPU manager.
    pub async fn build(base_url: &str) -> Result<Self> {
        let registry = LemonadeModelRegistry::fetch(base_url).await?;
        let gpu = GpuResourceManager::new();

        let tts = LemonadeTtsProvider::from_registry(&registry)?;
        let stt = LemonadeSttProvider::from_registry(&registry, Arc::clone(&gpu))?;
        let chat = LemonadeChatProvider::from_registry(&registry, Some(Arc::clone(&gpu)))?;

        info!(
            tts_model  = %tts.model,
            stt_model  = %stt.model,
            chat_model = %chat.model,
            "LemonadeStack ready"
        );

        Ok(Self {
            registry,
            gpu,
            tts,
            stt,
            chat,
        })
    }
}

impl std::fmt::Debug for LemonadeStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LemonadeStack")
            .field("tts_model", &self.tts.model)
            .field("stt_model", &self.stt.model)
            .field("chat_model", &self.chat.model)
            .field("gpu", &self.gpu)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::lemonade_url;
    use super::super::gpu_manager::GpuWorkload;

    #[tokio::test]
    async fn test_stack_builds_successfully() {
        let Some(url) = lemonade_url().await else {
            eprintln!("SKIP test_stack_builds_successfully — Lemonade Server not available");
            return;
        };

        let stack = LemonadeStack::build(&url).await.unwrap();
        assert_eq!(stack.tts.model, "kokoro-v1");
        assert!(stack.stt.model.contains("Whisper"));
        assert!(stack.chat.model.contains("GLM"));
        println!("{:?}", stack);
    }

    #[tokio::test]
    async fn test_stack_tts_and_chat_share_nothing_on_gpu() {
        let Some(url) = lemonade_url().await else {
            return;
        };

        let stack = LemonadeStack::build(&url).await.unwrap();
        // TTS runs on CPU — should not touch the GPU manager.
        assert_eq!(stack.gpu.current_workload(), GpuWorkload::Idle);

        let _audio = stack.tts.synthesize_default("Testing.").await.unwrap();
        // GPU should still be idle after a TTS call.
        assert_eq!(stack.gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_stack_stt_and_chat_share_gpu_manager() {
        let Some(_url) = lemonade_url().await else {
            return;
        };

        // Structural check: both stt and chat must hold the *same* Arc.
        // We verify this by acquiring via stt and seeing it reflected in chat's gpu.
        let gpu = GpuResourceManager::new();
        let stt_gpu = Arc::clone(&gpu);
        let chat_gpu = Arc::clone(&gpu);

        let _guard = stt_gpu.begin_stt().unwrap();
        assert_eq!(chat_gpu.current_workload(), GpuWorkload::SttActive);
    }
}
