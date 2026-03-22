//! Hardware device abstraction layer.
//!
//! This module defines the common vocabulary that the
//! [`InferenceQueue`](crate::inference_queue::InferenceQueue) uses to route work
//! to the correct physical accelerator.
//!
//! # Model
//!
//! Every piece of hardware is represented as a [`DeviceWorker`].  A worker
//! advertises:
//! * which [`HardwareBackend`] it runs on (NPU, ROCm GPU, CPU, …)
//! * which [`DeviceCapability`] values it can service (embedding, STT, TTS, …)
//!
//! The inference queue holds a pool of workers.  When a job arrives the queue
//! finds the first worker that (a) supports the required capability and (b) is
//! free to accept new work, and dispatches the job there.
//!
//! # Sub-modules
//!
//! | Module | Hardware | Capabilities |
//! |--------|----------|--------------|
//! | [`npu`] | AMD NPU via Lemonade FLM | Embedding, Transcription |
//! | [`gpu`] | AMD GPU via ROCm + Lemonade | Transcription, TextGeneration |
//! | [`cpu`] | Host CPU via Lemonade | TextToSpeech |

pub mod cpu;
pub mod gpu;
pub mod npu;

// ── DeviceCapability ──────────────────────────────────────────────────────────

/// The kind of inference work a [`DeviceWorker`] can perform.
///
/// Each variant corresponds to one job-type channel in the
/// [`InferenceQueue`](crate::inference_queue::InferenceQueue).  A worker may
/// support more than one capability; the queue will assign it any job whose
/// capability is in the worker's advertised set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceCapability {
    /// Generate a dense vector embedding from text.
    ///
    /// Serviced by: NPU (`embed-gemma-300m-FLM`).
    Embedding,

    /// Convert audio bytes to a text transcript (speech-to-text).
    ///
    /// Serviced by: NPU (`whisper-v3-turbo-FLM`) **and** GPU ROCm whisper.
    /// Both workers compete on the same queue channel — the first free device wins.
    Transcription,

    /// Generate text from a prompt (LLM / chat completion).
    ///
    /// Serviced by: GPU ROCm (`LemonadeChatProvider`).
    TextGeneration,

    /// Convert text to synthesised speech audio bytes.
    ///
    /// Serviced by: CPU (`LemonadeTtsProvider` / Kokoro).
    TextToSpeech,

    /// Rerank a list of candidate documents by relevance to a query.
    ///
    /// Not yet implemented — placeholder for Phase 4.
    Reranking,
}

impl std::fmt::Display for DeviceCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Embedding => write!(f, "Embedding"),
            Self::Transcription => write!(f, "Transcription"),
            Self::TextGeneration => write!(f, "TextGeneration"),
            Self::TextToSpeech => write!(f, "TextToSpeech"),
            Self::Reranking => write!(f, "Reranking"),
        }
    }
}

// ── HardwareBackend ───────────────────────────────────────────────────────────

/// Physical or virtual compute backend that drives a [`DeviceWorker`].
///
/// This is primarily informational — used for logging, metrics, and debugging.
/// The queue does **not** use the backend to make routing decisions; it relies
/// solely on [`DeviceCapability`] for that.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HardwareBackend {
    /// AMD Neural Processing Unit running quantised FLM models via Lemonade.
    ///
    /// Dedicated silicon — does not contend with the GPU for resources.
    Npu,

    /// AMD GPU running via ROCm + Lemonade Server.
    ///
    /// STT and LLM inference share this backend and are serialised by
    /// [`GpuResourceManager`](crate::lemonade::GpuResourceManager).
    GpuRocm,

    /// NVIDIA GPU running via CUDA + Lemonade Server.
    ///
    /// Reserved for future use; no concrete implementation yet.
    GpuCuda,

    /// Host CPU — no dedicated accelerator.
    ///
    /// Used for models that run cheaply on the CPU (e.g. Kokoro TTS).
    Cpu,

    /// Pure HTTP remote API — no local hardware required.
    ///
    /// Useful for cloud provider integrations in later phases.
    Remote,
}

impl std::fmt::Display for HardwareBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Npu => write!(f, "AMD NPU"),
            Self::GpuRocm => write!(f, "AMD GPU (ROCm)"),
            Self::GpuCuda => write!(f, "NVIDIA GPU (CUDA)"),
            Self::Cpu => write!(f, "CPU"),
            Self::Remote => write!(f, "Remote HTTP"),
        }
    }
}

// ── DeviceWorker ──────────────────────────────────────────────────────────────

/// A logical inference device that can service one or more [`DeviceCapability`]
/// types.
///
/// Implementors hold the underlying Lemonade provider(s) appropriate for their
/// hardware and expose the metadata the queue needs for dispatch decisions.
///
/// # Implementing `DeviceWorker`
///
/// ```rust
/// use u_forge_ai::hardware::{DeviceCapability, DeviceWorker, HardwareBackend};
///
/// struct MyDevice { caps: Vec<DeviceCapability> }
///
/// impl DeviceWorker for MyDevice {
///     fn name(&self) -> &str { "My Device" }
///     fn backend(&self) -> HardwareBackend { HardwareBackend::Remote }
///     fn capabilities(&self) -> &[DeviceCapability] { &self.caps }
/// }
/// ```
pub trait DeviceWorker: Send + Sync {
    /// Human-readable identifier for this device.
    ///
    /// Used in log messages and debug output.  Examples:
    /// - `"AMD NPU (FLM)"`
    /// - `"AMD GPU (ROCm)"`
    /// - `"CPU (Kokoro TTS)"`
    fn name(&self) -> &str;

    /// The physical backend this device runs on.
    fn backend(&self) -> HardwareBackend;

    /// All capabilities this device can service.
    ///
    /// The returned slice is used by the queue to decide which job channels
    /// a background worker task should listen on.
    fn capabilities(&self) -> &[DeviceCapability];

    /// Returns `true` if this device supports the given capability.
    ///
    /// Default implementation performs a linear scan of [`capabilities`].
    /// Override for O(1) lookup if the capability set is large.
    ///
    /// [`capabilities`]: DeviceWorker::capabilities
    fn supports(&self, cap: &DeviceCapability) -> bool {
        self.capabilities().contains(cap)
    }

    /// Returns a one-line summary for logging / display.
    ///
    /// Format: `"{name} [{backend}] caps=[{cap1}, {cap2}, …]"`
    fn summary(&self) -> String {
        let caps = self
            .capabilities()
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} [{}] caps=[{}]", self.name(), self.backend(), caps)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyDevice {
        caps: Vec<DeviceCapability>,
    }

    impl DeviceWorker for DummyDevice {
        fn name(&self) -> &str {
            "Dummy"
        }
        fn backend(&self) -> HardwareBackend {
            HardwareBackend::Remote
        }
        fn capabilities(&self) -> &[DeviceCapability] {
            &self.caps
        }
    }

    #[test]
    fn test_supports_returns_true_for_registered_capability() {
        let d = DummyDevice {
            caps: vec![DeviceCapability::Embedding, DeviceCapability::Transcription],
        };
        assert!(d.supports(&DeviceCapability::Embedding));
        assert!(d.supports(&DeviceCapability::Transcription));
    }

    #[test]
    fn test_supports_returns_false_for_missing_capability() {
        let d = DummyDevice {
            caps: vec![DeviceCapability::Embedding],
        };
        assert!(!d.supports(&DeviceCapability::TextToSpeech));
        assert!(!d.supports(&DeviceCapability::TextGeneration));
        assert!(!d.supports(&DeviceCapability::Reranking));
    }

    #[test]
    fn test_summary_contains_name_backend_and_caps() {
        let d = DummyDevice {
            caps: vec![DeviceCapability::Embedding, DeviceCapability::Transcription],
        };
        let s = d.summary();
        assert!(s.contains("Dummy"), "summary missing name: {s}");
        assert!(s.contains("Remote HTTP"), "summary missing backend: {s}");
        assert!(
            s.contains("Embedding"),
            "summary missing Embedding cap: {s}"
        );
        assert!(
            s.contains("Transcription"),
            "summary missing Transcription cap: {s}"
        );
    }

    #[test]
    fn test_capability_display() {
        assert_eq!(DeviceCapability::Embedding.to_string(), "Embedding");
        assert_eq!(DeviceCapability::Transcription.to_string(), "Transcription");
        assert_eq!(
            DeviceCapability::TextGeneration.to_string(),
            "TextGeneration"
        );
        assert_eq!(DeviceCapability::TextToSpeech.to_string(), "TextToSpeech");
        assert_eq!(DeviceCapability::Reranking.to_string(), "Reranking");
    }

    #[test]
    fn test_backend_display() {
        assert_eq!(HardwareBackend::Npu.to_string(), "AMD NPU");
        assert_eq!(HardwareBackend::GpuRocm.to_string(), "AMD GPU (ROCm)");
        assert_eq!(HardwareBackend::GpuCuda.to_string(), "NVIDIA GPU (CUDA)");
        assert_eq!(HardwareBackend::Cpu.to_string(), "CPU");
        assert_eq!(HardwareBackend::Remote.to_string(), "Remote HTTP");
    }

    #[test]
    fn test_capability_equality() {
        assert_eq!(DeviceCapability::Embedding, DeviceCapability::Embedding);
        assert_ne!(DeviceCapability::Embedding, DeviceCapability::Transcription);
    }

    #[test]
    fn test_backend_equality() {
        assert_eq!(HardwareBackend::Npu, HardwareBackend::Npu);
        assert_ne!(HardwareBackend::Npu, HardwareBackend::GpuRocm);
    }

    #[test]
    fn test_empty_capabilities() {
        let d = DummyDevice { caps: vec![] };
        assert!(!d.supports(&DeviceCapability::Embedding));
        let s = d.summary();
        assert!(s.contains("caps=[]"), "expected empty caps in summary: {s}");
    }
}
