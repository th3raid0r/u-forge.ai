//! Audio-to-text transcription trait and MIME helper.
//!
//! The [`TranscriptionProvider`] trait and [`mime_for_filename`] utility live
//! here and are dependency-free.  The [`LemonadeTranscriptionProvider`]
//! implementation lives in [`crate::lemonade::transcription`] and is
//! re-exported below.

use anyhow::Result;
use async_trait::async_trait;

// ── Re-exports ────────────────────────────────────────────────────────────────
pub use crate::lemonade::transcription::LemonadeTranscriptionProvider;

// ── TranscriptionProvider trait ───────────────────────────────────────────────

/// Core trait for all audio-to-text transcription backends.
///
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks and placed behind an `Arc`.
///
/// # MIME inference
///
/// The `filename` parameter is used as a hint to determine the audio MIME type
/// for the multipart upload.  Recognised extensions:
///
/// | Extension | MIME type    |
/// |-----------|--------------|
/// | `.mp3`    | `audio/mpeg` |
/// | `.ogg`    | `audio/ogg`  |
/// | anything  | `audio/wav`  |
#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    /// Transcribe raw audio bytes to text.
    ///
    /// `audio_bytes` — contents of a valid audio file (WAV, MP3, OGG, …).
    /// `filename`    — sent as the multipart filename hint (e.g. `"session.wav"`).
    ///
    /// Returns the transcribed text trimmed of leading/trailing whitespace.
    async fn transcribe(&self, audio_bytes: Vec<u8>, filename: &str) -> Result<String>;

    /// The model name this provider is configured to use.
    fn model_name(&self) -> &str;
}

// ── MIME helper (public for reuse in lemonade modules) ───────────────────────

/// Infer the audio MIME type from a filename extension via [`mime_guess`].
///
/// Falls back to `"audio/wav"` when the extension is unrecognised.
pub fn mime_for_filename(filename: &str) -> String {
    mime_guess::from_path(filename)
        .first()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "audio/wav".to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::require_integration_url;

    // ── WAV helper ────────────────────────────────────────────────────────────

    /// Build a minimal valid PCM WAV file containing silence.
    ///
    /// Parameters: mono, 16-bit, 16 kHz.  `duration_secs` controls length.
    /// No external dependencies — pure byte construction following the RIFF spec.
    pub(crate) fn make_silence_wav(duration_secs: f32) -> Vec<u8> {
        let sample_rate: u32 = 16_000;
        let num_channels: u16 = 1;
        let bits_per_sample: u16 = 16;
        let num_samples = (sample_rate as f32 * duration_secs) as u32;
        let data_size = num_samples * (bits_per_sample as u32 / 8) * num_channels as u32;
        // RIFF chunk size = 4 (WAVE) + 8 (fmt hdr) + 16 (fmt body) + 8 (data hdr) + data
        let riff_size: u32 = 4 + 8 + 16 + 8 + data_size;

        let mut buf: Vec<u8> = Vec::with_capacity((8 + riff_size) as usize);

        // RIFF header
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt sub-chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size (PCM)
        buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
        buf.extend_from_slice(&num_channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate: u32 = sample_rate * num_channels as u32 * bits_per_sample as u32 / 8;
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align: u16 = num_channels * bits_per_sample / 8;
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data sub-chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.extend(std::iter::repeat_n(0u8, data_size as usize));

        buf
    }

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_lemonade_transcription_provider_model_name() {
        let p = LemonadeTranscriptionProvider::new(
            "http://localhost:13305/api/v1",
            "whisper-v3-turbo-FLM",
        );
        assert_eq!(p.model_name(), "whisper-v3-turbo-FLM");
    }

    #[test]
    fn test_provider_trims_trailing_slash_from_url() {
        let p = LemonadeTranscriptionProvider::new(
            "http://localhost:13305/api/v1/",
            "whisper-v3-turbo-FLM",
        );
        // base_url should not end in '/'
        assert!(
            !p.client.base_url.ends_with('/'),
            "base_url should not end with '/': {}",
            p.client.base_url
        );
    }

    #[test]
    fn test_make_silence_wav_valid_header() {
        let wav = make_silence_wav(0.1);
        // RIFF magic
        assert_eq!(&wav[0..4], b"RIFF", "Missing RIFF magic");
        // WAVE fourcc
        assert_eq!(&wav[8..12], b"WAVE", "Missing WAVE fourcc");
        // fmt sub-chunk
        assert_eq!(&wav[12..16], b"fmt ", "Missing fmt chunk");
        // Audio format = 1 (PCM)
        let audio_fmt = u16::from_le_bytes([wav[20], wav[21]]);
        assert_eq!(audio_fmt, 1, "Expected PCM format (1), got {audio_fmt}");
        // Sample rate = 16 000
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16_000, "Expected 16 kHz sample rate, got {sr}");
        // Channels = 1 (mono)
        let ch = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(ch, 1, "Expected mono (1), got {ch}");
    }

    #[test]
    fn test_make_silence_wav_size_scaling() {
        let wav_short = make_silence_wav(0.5);
        let wav_long = make_silence_wav(1.0);
        assert!(
            wav_long.len() > wav_short.len(),
            "Longer duration must produce a larger file"
        );
        // 1.0 s should be roughly double 0.5 s (within header overhead)
        let ratio = wav_long.len() as f64 / wav_short.len() as f64;
        assert!(
            ratio > 1.8 && ratio < 2.2,
            "Expected ~2× size ratio, got {ratio:.2}"
        );
    }

    #[test]
    fn test_mime_for_filename() {
        assert_eq!(mime_for_filename("track.mp3"), "audio/mpeg");
        assert_eq!(mime_for_filename("session.ogg"), "audio/ogg");
        assert_eq!(mime_for_filename("recording.flac"), "audio/flac");
        assert_eq!(mime_for_filename("voice.m4a"), "audio/m4a");
        assert_eq!(mime_for_filename("audio.wav"), "audio/wav");
        assert_eq!(mime_for_filename("unknown.zzz"), "audio/wav");
        // Case-insensitive
        assert_eq!(mime_for_filename("TRACK.MP3"), "audio/mpeg");
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_lemonade_transcribe_silence_wav() {
        let url = require_integration_url!();
        let provider = LemonadeTranscriptionProvider::new(&url, "whisper-v3-turbo-FLM");

        // 1 second of silence — valid WAV, no speech content.
        let wav = make_silence_wav(1.0);
        let result = provider.transcribe(wav, "silence.wav").await;
        assert!(
            result.is_ok(),
            "transcribe() failed on silence WAV: {:?}",
            result.err()
        );
        // May be empty or contain hallucinated noise words — both are acceptable.
        let _text = result.unwrap();
    }

    #[tokio::test]
    async fn test_lemonade_transcribe_error_on_empty_body() {
        let url = require_integration_url!();
        let provider = LemonadeTranscriptionProvider::new(&url, "whisper-v3-turbo-FLM");

        // Sending an empty byte slice — the server should return an error.
        let result = provider.transcribe(vec![], "empty.wav").await;
        assert!(
            result.is_err(),
            "Expected error for empty audio, got Ok: {:?}",
            result.ok()
        );
    }
}
