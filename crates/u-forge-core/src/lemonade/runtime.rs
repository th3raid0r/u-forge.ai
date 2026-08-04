//! Serialized Lemonade LLM runtime-profile activation.

use anyhow::Result;
use tokio::sync::Mutex;

use super::{ModelLoadOptions, reload_model};

/// State that determines whether Lemonade must reload the active LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LemonadeRuntimeProfile {
    pub model_id: String,
    pub reasoning_enabled: bool,
    pub load_options: ModelLoadOptions,
}

impl LemonadeRuntimeProfile {
    pub fn new(
        model_id: impl Into<String>,
        reasoning_enabled: bool,
        load_options: ModelLoadOptions,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            reasoning_enabled,
            load_options,
        }
    }
}

/// Coordinates Lemonade's single active LLM profile.
///
/// The lock intentionally remains held across `/load`: model switches and
/// reasoning toggles are global server state and must not race each other.
pub struct LemonadeRuntime {
    base_url: String,
    active: Mutex<Option<LemonadeRuntimeProfile>>,
}

impl LemonadeRuntime {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            active: Mutex::new(None),
        }
    }

    /// Ensure `profile` is active. Returns `true` when a reload was performed.
    pub async fn activate(&self, profile: &LemonadeRuntimeProfile) -> Result<bool> {
        let mut active = self.active.lock().await;
        if active.as_ref() == Some(profile) {
            return Ok(false);
        }

        reload_model(&self.base_url, &profile.model_id, &profile.load_options).await?;
        *active = Some(profile.clone());
        Ok(true)
    }

    pub async fn active_profile(&self) -> Option<LemonadeRuntimeProfile> {
        self.active.lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_mode_is_part_of_runtime_identity() {
        let normal = LemonadeRuntimeProfile::new("model", false, ModelLoadOptions::default());
        let reasoning = LemonadeRuntimeProfile::new("model", true, ModelLoadOptions::default());
        assert_ne!(normal, reasoning);
    }

    #[test]
    fn load_options_are_part_of_runtime_identity() {
        let first = LemonadeRuntimeProfile::new(
            "model",
            false,
            ModelLoadOptions {
                ctx_size: Some(4096),
                ..Default::default()
            },
        );
        let second = LemonadeRuntimeProfile::new(
            "model",
            false,
            ModelLoadOptions {
                ctx_size: Some(8192),
                ..Default::default()
            },
        );
        assert_ne!(first, second);
    }
}
