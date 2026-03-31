//! Inference request queue — async job dispatch to AI providers.
//!
//! The [`InferenceQueue`] accepts embedding, transcription, and TTS jobs from
//! any number of callers and dispatches each job to whichever registered device
//! worker is both **capable** of handling it and **free** to accept new work.
//!
//! # Design
//!
//! Each capability type (Embedding, Transcription, TextToSpeech) has its own
//! shared work queue — a `Mutex<VecDeque<_>>` guarded by a `Notify` semaphore.
//! When the queue is built, one Tokio background task is spawned per
//! (device, capability) pair.  Multiple tasks listening on the same queue
//! implement work-stealing: whichever worker finishes first picks up the next
//! job.
//!
//! ```text
//!   caller                InferenceQueue           device workers (Tokio tasks)
//!   ──────                ──────────────           ────────────────────────────
//!
//!   embed(text)  ───────► embed_queue  ──────────► NpuDevice  (embed-gemma-300m-FLM)
//!                                      ──────────► llamacpp   (embeddinggemma-300M-GGUF)
//!
//!   transcribe() ───────► transcribe_queue ──────► NpuDevice  (whisper-v3-turbo-FLM)
//!                                         ──────► GpuDevice  (Whisper-Large-v3-Turbo)
//!
//!   synthesize() ───────► synthesize_queue ──────► CpuDevice  (kokoro-v1)
//! ```
//!
//! The race is natural: both transcription workers receive a `notify_one()` when
//! a job is pushed, but only one can pop the job from the `VecDeque`.  The other
//! sees an empty queue and goes back to sleep.  If both workers are busy, the job
//! waits in the deque until one finishes.
//!
//! # Race-free wakeup
//!
//! The worker loop registers a [`Notified`](tokio::sync::futures::Notified) future
//! **before** checking the queue.  This prevents the classic lost-wakeup race:
//!
//! ```text
//! loop {
//!     let notified = queue.notify.notified();   // register first
//!     if let Some(job) = queue.try_pop() {       // then check
//!         drop(notified);
//!         process(job).await;
//!     } else {
//!         notified.await;                        // sleep if nothing found
//!     }
//! }
//! ```
//!
//! If a job is pushed between `notified()` and `try_pop()`, the deque will be
//! non-empty and the worker processes it immediately.  If the push happens after
//! `try_pop()` returns `None` but before `.await`, the stored `Notify` permit
//! ensures the worker wakes on the next poll.
//!
//! # Usage
//!
//! ```no_run
//! # use u_forge_core::queue::InferenceQueueBuilder;
//! # use u_forge_core::hardware::npu::NpuDevice;
//! # use u_forge_core::hardware::gpu::GpuDevice;
//! # use u_forge_core::hardware::cpu::CpuDevice;
//! # async fn run() -> anyhow::Result<()> {
//! # let npu_device: NpuDevice = todo!();
//! # let gpu_device: GpuDevice = todo!();
//! # let cpu_device: CpuDevice = todo!();
//! # let wav_bytes: Vec<u8> = Vec::new();
//! let queue = InferenceQueueBuilder::new()
//!     .with_npu_device(npu_device)
//!     .with_gpu_device(gpu_device)
//!     .with_cpu_device(cpu_device)
//!     .build();
//!
//! // Embeddings go to the NPU
//! let vec = queue.embed("The kingdom fell at dawn.").await?;
//!
//! // Transcription goes to whichever of NPU / GPU is free first
//! let text = queue.transcribe(wav_bytes, "session.wav").await?;
//!
//! // TTS goes to the CPU
//! let audio = queue.synthesize("Welcome, adventurer!", None).await?;
//! # Ok(()) }
//! ```

mod builder;
mod dispatch;
mod jobs;
mod weighted;
mod workers;

pub use builder::InferenceQueueBuilder;
pub use dispatch::{InferenceQueue, QueueStats};
