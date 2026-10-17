//! # Voice-to-Intent Engine (`voice_loop`)
//!
//! Production-grade voice capture, PCM/WAV binary parsing, Voice Activity
//! Detection (VAD), Whisper transcription client, and natural language
//! voice-to-intent classification.

use std::fmt;
use std::time::Duration;
use serde::{Deserialize, Serialize};

/// 44-byte standard RIFF/WAVE PCM Header
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WavHeader {
    pub chunk_id: [u8; 4],       // "RIFF"
    pub chunk_size: u32,         // 36 + subchunk2_size
    pub format: [u8; 4],           // "WAVE"
    pub subchunk1_id: [u8; 4],   // "fmt "
    pub subchunk1_size: u32,     // 16 for PCM
    pub audio_format: u16,       // 1 for PCM linear
    pub num_channels: u16,       // 1 = mono, 2 = stereo
    pub sample_rate: u32,        // e.g. 16000, 44100, 48000
    pub byte_rate: u32,          // sample_rate * num_channels * bits_per_sample / 8
    pub block_align: u16,        // num_channels * bits_per_sample / 8
    pub bits_per_sample: u16,    // 16 for standard 16-bit signed PCM
    pub subchunk2_id: [u8; 4],   // "data"
    pub subchunk2_size: u32,     // data byte count
}

/// Parsed Audio stream representation
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAudio {
    pub header: WavHeader,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>, // Normalized to [-1.0, 1.0]
    pub duration_seconds: f32,
}

impl WavHeader {
    /// Create standard 16-bit PCM WAV header for given audio dimensions
    pub fn new_pcm16(num_channels: u16, sample_rate: u32, num_samples: usize) -> Self {
        let subchunk2_size = (num_samples * (num_channels as usize) * 2) as u32;
        let chunk_size = 36 + subchunk2_size;
        let byte_rate = sample_rate * (num_channels as u32) * 2;
        let block_align = num_channels * 2;

        Self {
            chunk_id: *b"RIFF",
            chunk_size,
            format: *b"WAVE",
            subchunk1_id: *b"fmt ",
            subchunk1_size: 16,
            audio_format: 1, // PCM
            num_channels,
            sample_rate,
            byte_rate,
            block_align,
            bits_per_sample: 16,
            subchunk2_id: *b"data",
            subchunk2_size,
        }
    }

    /// Convert header to 44-byte binary representation
    pub fn to_bytes(&self) -> [u8; 44] {
        let mut buf = [0u8; 44];
        buf[0..4].copy_from_slice(&self.chunk_id);
        buf[4..8].copy_from_slice(&self.chunk_size.to_le_bytes());
        buf[8..12].copy_from_slice(&self.format);
        buf[12..16].copy_from_slice(&self.subchunk1_id);
        buf[16..20].copy_from_slice(&self.subchunk1_size.to_le_bytes());
        buf[20..22].copy_from_slice(&self.audio_format.to_le_bytes());
        buf[22..24].copy_from_slice(&self.num_channels.to_le_bytes());
        buf[24..28].copy_from_slice(&self.sample_rate.to_le_bytes());
        buf[28..32].copy_from_slice(&self.byte_rate.to_le_bytes());
        buf[32..34].copy_from_slice(&self.block_align.to_le_bytes());
        buf[34..36].copy_from_slice(&self.bits_per_sample.to_le_bytes());
        buf[36..40].copy_from_slice(&self.subchunk2_id);
        buf[40..44].copy_from_slice(&self.subchunk2_size.to_le_bytes());
        buf
    }

    /// Parse header from 44-byte slice
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 44 {
            return Err(format!("Byte buffer too short for WAV header: {} bytes (required 44)", bytes.len()));
        }

        let mut chunk_id = [0u8; 4];
        chunk_id.copy_from_slice(&bytes[0..4]);
        if &chunk_id != b"RIFF" {
            return Err(format!("Invalid RIFF identifier: {:?}", chunk_id));
        }

        let chunk_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        let mut format = [0u8; 4];
        format.copy_from_slice(&bytes[8..12]);
        if &format != b"WAVE" {
            return Err(format!("Invalid WAVE format identifier: {:?}", format));
        }

        let mut subchunk1_id = [0u8; 4];
        subchunk1_id.copy_from_slice(&bytes[12..16]);
        if &subchunk1_id != b"fmt " {
            return Err(format!("Missing fmt subchunk: {:?}", subchunk1_id));
        }

        let subchunk1_size = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let audio_format = u16::from_le_bytes([bytes[20], bytes[21]]);
        let num_channels = u16::from_le_bytes([bytes[22], bytes[23]]);
        let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        let byte_rate = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
        let block_align = u16::from_le_bytes([bytes[32], bytes[33]]);
        let bits_per_sample = u16::from_le_bytes([bytes[34], bytes[35]]);

        let mut subchunk2_id = [0u8; 4];
        subchunk2_id.copy_from_slice(&bytes[36..40]);
        let subchunk2_size = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);

        Ok(Self {
            chunk_id,
            chunk_size,
            format,
            subchunk1_id,
            subchunk1_size,
            audio_format,
            num_channels,
            sample_rate,
            byte_rate,
            block_align,
            bits_per_sample,
            subchunk2_id,
            subchunk2_size,
        })
    }
}

/// Real WAV parser converting raw PCM bytes into normalized float samples
pub fn parse_wav(bytes: &[u8]) -> Result<ParsedAudio, String> {
    if bytes.len() < 44 {
        return Err("Audio buffer too small for standard WAV file".to_string());
    }

    let header = WavHeader::from_bytes(bytes)?;
    let data_offset = 44;
    let data_bytes = if bytes.len() >= data_offset + (header.subchunk2_size as usize) {
        &bytes[data_offset..data_offset + (header.subchunk2_size as usize)]
    } else {
        &bytes[data_offset..]
    };

    let mut samples = Vec::new();
    match header.bits_per_sample {
        16 => {
            for chunk in data_bytes.chunks_exact(2) {
                let sample_i16 = i16::from_le_bytes([chunk[0], chunk[1]]);
                let sample_f32 = (sample_i16 as f32) / 32768.0;
                samples.push(sample_f32);
            }
        }
        8 => {
            for &byte in data_bytes {
                let sample_f32 = ((byte as f32) - 128.0) / 128.0;
                samples.push(sample_f32);
            }
        }
        32 => {
            for chunk in data_bytes.chunks_exact(4) {
                let sample_f32 = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                samples.push(sample_f32);
            }
        }
        other => {
            return Err(format!("Unsupported bit depth: {} bits", other));
        }
    }

    let sample_rate = header.sample_rate;
    let total_samples = samples.len();
    let channels = header.num_channels.max(1);
    let frames = total_samples / (channels as usize);
    let duration_seconds = (frames as f32) / (sample_rate as f32);

    Ok(ParsedAudio {
        header,
        sample_rate,
        channels,
        samples,
        duration_seconds,
    })
}

/// Encode normalized float audio samples into standard 16-bit PCM WAV byte stream
pub fn encode_wav_pcm16(samples: &[f32], sample_rate: u32, num_channels: u16) -> Vec<u8> {
    let header = WavHeader::new_pcm16(num_channels, sample_rate, samples.len());
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(&header.to_bytes());

    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let sample_i16 = if clamped >= 0.0 {
            (clamped * 32767.0) as i16
        } else {
            (clamped * 32768.0) as i16
        };
        out.extend_from_slice(&sample_i16.to_le_bytes());
    }
    out
}

/// Voice Activity Detection (VAD) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// Frame size in milliseconds (typically 10ms - 30ms)
    pub frame_duration_ms: usize,
    /// RMS energy threshold to count as active speech
    pub energy_threshold: f32,
    /// Minimum consecutive active frames to trigger speech onset
    pub min_speech_frames: usize,
    /// Consecutive silence frames required to detect speech offset
    pub silence_padding_frames: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_duration_ms: 20,
            energy_threshold: 0.015,
            min_speech_frames: 3,
            silence_padding_frames: 10,
        }
    }
}

/// Detected continuous speech segment
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeechSegment {
    pub start_sample: usize,
    pub end_sample: usize,
    pub start_sec: f32,
    pub end_sec: f32,
    pub peak_rms: f32,
    pub mean_rms: f32,
}

/// Compute Root Mean Square (RMS) energy for an audio frame
pub fn compute_frame_rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frame.iter().map(|&x| x * x).sum();
    (sum_sq / (frame.len() as f32)).sqrt()
}

/// Voice Activity Detector
pub struct VoiceActivityDetector {
    pub config: VadConfig,
}

impl VoiceActivityDetector {
    pub fn new(config: VadConfig) -> Self {
        Self { config }
    }

    /// Segment audio into active speech regions based on RMS energy thresholding
    pub fn detect_segments(&self, samples: &[f32], sample_rate: u32) -> Vec<SpeechSegment> {
        if samples.is_empty() || sample_rate == 0 {
            return Vec::new();
        }

        let frame_samples = ((sample_rate as usize) * self.config.frame_duration_ms) / 1000;
        if frame_samples == 0 {
            return Vec::new();
        }

        let mut segments = Vec::new();
        let mut in_speech = false;
        let mut speech_start_sample = 0;
        let mut active_frames_count = 0;
        let mut silence_frames_count = 0;

        let mut current_frame_rms = Vec::new();

        for (frame_idx, chunk) in samples.chunks(frame_samples).enumerate() {
            let rms = compute_frame_rms(chunk);
            let frame_sample_start = frame_idx * frame_samples;

            if rms >= self.config.energy_threshold {
                active_frames_count += 1;
                silence_frames_count = 0;

                if !in_speech && active_frames_count >= self.config.min_speech_frames {
                    in_speech = true;
                    speech_start_sample = frame_sample_start.saturating_sub(
                        (self.config.min_speech_frames - 1) * frame_samples
                    );
                    current_frame_rms.clear();
                }

                if in_speech {
                    current_frame_rms.push(rms);
                }
            } else {
                active_frames_count = 0;
                if in_speech {
                    silence_frames_count += 1;
                    current_frame_rms.push(rms);

                    if silence_frames_count >= self.config.silence_padding_frames {
                        let end_sample = (frame_sample_start + chunk.len()).min(samples.len());
                        let peak_rms = current_frame_rms.iter().cloned().fold(0.0f32, f32::max);
                        let mean_rms = if current_frame_rms.is_empty() {
                            0.0
                        } else {
                            current_frame_rms.iter().sum::<f32>() / (current_frame_rms.len() as f32)
                        };

                        segments.push(SpeechSegment {
                            start_sample: speech_start_sample,
                            end_sample,
                            start_sec: (speech_start_sample as f32) / (sample_rate as f32),
                            end_sec: (end_sample as f32) / (sample_rate as f32),
                            peak_rms,
                            mean_rms,
                        });

                        in_speech = false;
                        current_frame_rms.clear();
                    }
                }
            }
        }

        if in_speech {
            let end_sample = samples.len();
            let peak_rms = current_frame_rms.iter().cloned().fold(0.0f32, f32::max);
            let mean_rms = if current_frame_rms.is_empty() {
                0.0
            } else {
                current_frame_rms.iter().sum::<f32>() / (current_frame_rms.len() as f32)
            };

            segments.push(SpeechSegment {
                start_sample: speech_start_sample,
                end_sample,
                start_sec: (speech_start_sample as f32) / (sample_rate as f32),
                end_sec: (end_sample as f32) / (sample_rate as f32),
                peak_rms,
                mean_rms,
            });
        }

        segments
    }

    /// Trim leading and trailing silence, returning only active speech samples
    pub fn trim_silence(&self, samples: &[f32], sample_rate: u32) -> Vec<f32> {
        let segments = self.detect_segments(samples, sample_rate);
        if segments.is_empty() {
            return Vec::new();
        }

        let first_start = segments.first().map(|s| s.start_sample).unwrap_or(0);
        let last_end = segments.last().map(|s| s.end_sample).unwrap_or(samples.len()).min(samples.len());

        if first_start < last_end && last_end <= samples.len() {
            samples[first_start..last_end].to_vec()
        } else {
            samples.to_vec()
        }
    }
}

/// Transcription Client targeting Local Whisper or Cloud Whisper (Groq/OpenAI)
#[derive(Debug, Clone)]
pub struct VoiceTranscriptionClient {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model: String,
    pub timeout_seconds: u64,
}

impl Default for VoiceTranscriptionClient {
    fn default() -> Self {
        if let Ok(key) = std::env::var("GROQ_API_KEY") {
            Self {
                endpoint: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                api_key: Some(key),
                model: "whisper-large-v3".to_string(),
                timeout_seconds: 30,
            }
        } else if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            Self {
                endpoint: "https://api.openai.com/v1/audio/transcriptions".to_string(),
                api_key: Some(key),
                model: "whisper-1".to_string(),
                timeout_seconds: 30,
            }
        } else {
            Self {
                endpoint: "http://127.0.0.1:8080/inference".to_string(),
                api_key: None,
                model: "whisper.cpp".to_string(),
                timeout_seconds: 30,
            }
        }
    }
}

impl VoiceTranscriptionClient {
    pub fn new(endpoint: &str, api_key: Option<String>, model: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            api_key,
            model: model.to_string(),
            timeout_seconds: 30,
        }
    }

    /// Transcribe WAV byte buffer using HTTP POST
    pub async fn transcribe(&self, wav_bytes: Vec<u8>) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_seconds))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        if self.endpoint.contains("8080/inference") || self.endpoint.ends_with("/inference") {
            let part = reqwest::multipart::Part::bytes(wav_bytes)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| e.to_string())?;

            let form = reqwest::multipart::Form::new()
                .part("file", part)
                .text("temperature", "0.0")
                .text("response_format", "json");

            let res = client
                .post(&self.endpoint)
                .multipart(form)
                .send()
                .await
                .map_err(|e| format!("Local Whisper server connection error: {}", e))?;

            let status = res.status();
            let body = res.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("Local Whisper returned HTTP {}: {}", status, body));
            }

            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(text) = val.get("text").and_then(|t| t.as_str()) {
                    return Ok(text.trim().to_string());
                }
            }
            return Ok(body.trim().to_string());
        }

        let part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| e.to_string())?;

        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("response_format", "json");

        let mut req = client.post(&self.endpoint).multipart(form);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }

        let res = req.send().await.map_err(|e| format!("Whisper API connection error: {}", e))?;
        let status = res.status();
        let body = res.text().await.map_err(|e| e.to_string())?;

        if !status.is_success() {
            return Err(format!("Whisper API returned HTTP {}: {}", status, body));
        }

        let val: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("Failed to parse Whisper JSON response: {} (Body: {})", e, body))?;

        val.get("text")
            .and_then(|t| t.as_str())
            .map(|t| t.trim().to_string())
            .ok_or_else(|| format!("'text' field missing in Whisper response: {}", body))
    }
}

/// Structured Voice Intent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceIntent {
    /// Refactor specific code target to accomplish a goal
    Refactor { target: String, goal: String },
    /// Add test for target module/function
    AddTest { target: String, test_type: String },
    /// Fix a specific bug or issue
    FixBug { description: String },
    /// Explain architecture, symbol, or file
    Explain { symbol_or_file: String },
    /// Execute a CLI command directly
    ExecuteCommand { command: String },
    /// Fallback for unclassified utterances
    Unknown(String),
}

impl fmt::Display for VoiceIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refactor { target, goal } => write!(f, "Refactor target '{}' with goal '{}'", target, goal),
            Self::AddTest { target, test_type } => write!(f, "Add {} test for '{}'", test_type, target),
            Self::FixBug { description } => write!(f, "Fix bug: {}", description),
            Self::Explain { symbol_or_file } => write!(f, "Explain: {}", symbol_or_file),
            Self::ExecuteCommand { command } => write!(f, "Execute command: `{}`", command),
            Self::Unknown(raw) => write!(f, "Unknown intent: '{}'", raw),
        }
    }
}

/// Natural Language Voice Intent Classifier
pub struct VoiceIntentClassifier;

impl VoiceIntentClassifier {
    /// Classify a spoken transcription string into a structured VoiceIntent
    pub fn classify(transcription: &str) -> VoiceIntent {
        let text = transcription.trim();
        if text.is_empty() {
            return VoiceIntent::Unknown(String::new());
        }

        let lower = text.trim_end_matches(['.', '!', '?']).to_lowercase();
        let normalized = lower.replace([',', '!', '?', ';', '"', '\''], "");

        // 1. Refactor intent
        if normalized.starts_with("refactor ") || normalized.starts_with("rewrite ") || normalized.starts_with("clean up ") {
            let remainder = if let Some(stripped) = normalized.strip_prefix("refactor ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("rewrite ") {
                stripped
            } else {
                normalized.strip_prefix("clean up ").unwrap_or(&normalized)
            };

            let parts: Vec<&str> = remainder.split(" to ").collect();
            if parts.len() >= 2 {
                let target = parts[0].trim().to_string();
                let goal = parts[1..].join(" to ").trim().to_string();
                return VoiceIntent::Refactor { target, goal };
            }

            let parts: Vec<&str> = remainder.split(" with ").collect();
            if parts.len() >= 2 {
                let target = parts[0].trim().to_string();
                let goal = format!("with {}", parts[1..].join(" with ").trim());
                return VoiceIntent::Refactor { target, goal };
            }

            let parts: Vec<&str> = remainder.split(" using ").collect();
            if parts.len() >= 2 {
                let target = parts[0].trim().to_string();
                let goal = format!("using {}", parts[1..].join(" using ").trim());
                return VoiceIntent::Refactor { target, goal };
            }

            return VoiceIntent::Refactor {
                target: remainder.trim().to_string(),
                goal: "improve code quality and ergonomics".to_string(),
            };
        }

        // 2. Add Test intent
        if normalized.starts_with("add test") || normalized.starts_with("add unit test") || normalized.starts_with("add integration test")
            || normalized.starts_with("write test") || normalized.starts_with("write unit test") || normalized.starts_with("write integration test")
            || normalized.starts_with("generate test") || normalized.starts_with("generate unit test")
            || (normalized.starts_with("add ") && normalized.contains("test"))
            || (normalized.starts_with("write ") && normalized.contains("test"))
            || (normalized.starts_with("generate ") && normalized.contains("test"))
            || normalized.starts_with("test ") {
            let test_type = if normalized.contains("integration") {
                "integration".to_string()
            } else if normalized.contains("regression") {
                "regression".to_string()
            } else if normalized.contains("fuzz") {
                "fuzz".to_string()
            } else {
                "unit".to_string()
            };

            let target = if let Some(idx) = normalized.find(" for ") {
                normalized[idx + 5..].trim().to_string()
            } else if let Some(stripped) = normalized.strip_prefix("test ") {
                stripped.trim().to_string()
            } else {
                "current workspace".to_string()
            };

            return VoiceIntent::AddTest { target, test_type };
        }

        // 3. Fix Bug intent
        if normalized.starts_with("fix ") || normalized.starts_with("solve ") || normalized.starts_with("resolve ") || normalized.starts_with("repair ") {
            let desc = if let Some(stripped) = normalized.strip_prefix("fix bug ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("fix the bug ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("fix issue ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("fix the issue ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("fix ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("solve ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("resolve ") {
                stripped
            } else {
                normalized.strip_prefix("repair ").unwrap_or(&normalized)
            };

            return VoiceIntent::FixBug {
                description: desc.trim().to_string(),
            };
        }

        // 4. Explain intent
        if normalized.starts_with("explain ") || normalized.starts_with("how does ") || normalized.starts_with("what is ") || normalized.starts_with("describe ") {
            let target = if let Some(stripped) = normalized.strip_prefix("explain ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("describe ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("what is ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("how does ") {
                stripped.trim_end_matches(" work").trim()
            } else {
                &normalized
            };

            return VoiceIntent::Explain {
                symbol_or_file: target.trim().to_string(),
            };
        }

        // 5. Execute Command intent
        if normalized.starts_with("run ") || normalized.starts_with("execute ") || normalized.starts_with("build ") || normalized.starts_with("cargo ") {
            let cmd = if let Some(stripped) = normalized.strip_prefix("run command ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("run ") {
                stripped
            } else if let Some(stripped) = normalized.strip_prefix("execute ") {
                stripped
            } else {
                &normalized
            };

            let command = if cmd == "build" || cmd == "build project" {
                "cargo build".to_string()
            } else if cmd == "test" || cmd == "test all" {
                "cargo test".to_string()
            } else {
                cmd.trim().to_string()
            };

            return VoiceIntent::ExecuteCommand { command };
        }

        VoiceIntent::Unknown(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wav_header_and_pcm16_roundtrip() {
        let sample_rate = 16000;
        let channels = 1;
        let mut samples = Vec::new();
        for i in 0..sample_rate {
            let t = (i as f32) / (sample_rate as f32);
            let s = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.75;
            samples.push(s);
        }

        let wav_bytes = encode_wav_pcm16(&samples, sample_rate, channels);
        assert!(wav_bytes.len() >= 44 + sample_rate as usize * 2);

        let parsed = parse_wav(&wav_bytes).expect("Failed to parse encoded WAV");
        assert_eq!(parsed.sample_rate, 16000);
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.samples.len(), samples.len());
        assert!((parsed.duration_seconds - 1.0).abs() < 0.01);

        for (orig, decoded) in samples.iter().zip(parsed.samples.iter()) {
            assert!((orig - decoded).abs() < 0.001);
        }
    }

    #[test]
    fn test_voice_activity_detection_silence_filtering() {
        let sample_rate = 16000;
        let mut samples = Vec::new();

        samples.extend(vec![0.0f32; 8000]);

        for i in 0..16000 {
            let t = (i as f32) / 16000.0;
            samples.push((t * 200.0 * 2.0 * std::f32::consts::PI).sin() * 0.5);
        }

        samples.extend(vec![0.0f32; 8000]);

        let vad = VoiceActivityDetector::new(VadConfig::default());
        let segments = vad.detect_segments(&samples, sample_rate);

        assert!(!segments.is_empty(), "Expected speech segments to be detected");
        let first = &segments[0];
        assert!(first.start_sec >= 0.45 && first.start_sec <= 0.65, "Start was {}", first.start_sec);
        assert!(first.end_sec >= 1.45 && first.end_sec <= 1.75, "End was {}", first.end_sec);
        assert!(first.peak_rms > 0.1);

        let trimmed = vad.trim_silence(&samples, sample_rate);
        assert!(trimmed.len() < samples.len());
        assert!(trimmed.len() >= 16000);
    }

    #[test]
    fn test_voice_intent_classification() {
        let intent1 = VoiceIntentClassifier::classify("Refactor function parse_tokens to use zero-copy iterators");
        assert_eq!(
            intent1,
            VoiceIntent::Refactor {
                target: "function parse_tokens".to_string(),
                goal: "use zero-copy iterators".to_string()
            }
        );

        let intent2 = VoiceIntentClassifier::classify("Add unit test for voice engine");
        assert_eq!(
            intent2,
            VoiceIntent::AddTest {
                target: "voice engine".to_string(),
                test_type: "unit".to_string()
            }
        );

        let intent3 = VoiceIntentClassifier::classify("Fix bug memory leak in websocket packet buffer");
        assert_eq!(
            intent3,
            VoiceIntent::FixBug {
                description: "memory leak in websocket packet buffer".to_string()
            }
        );

        let intent4 = VoiceIntentClassifier::classify("Explain ast_query.rs and how invariants work");
        assert_eq!(
            intent4,
            VoiceIntent::Explain {
                symbol_or_file: "ast_query.rs and how invariants work".to_string()
            }
        );

        let intent5 = VoiceIntentClassifier::classify("Run cargo test --all");
        assert_eq!(
            intent5,
            VoiceIntent::ExecuteCommand {
                command: "cargo test --all".to_string()
            }
        );

        let intent6 = VoiceIntentClassifier::classify("Good morning, have a nice day");
        assert!(matches!(intent6, VoiceIntent::Unknown(_)));
    }
}
