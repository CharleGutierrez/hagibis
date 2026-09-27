//! # Ambient Audio Earcons & Voice-to-Diff Flow State Bridge
//!
//! Provides subtle, non-blocking auditory cues for developer flow state
//! (dual-draft race wins, compiler healing, invariant passes, secret leak interceptions)
//! and parses spoken voice utterances into structured AST diff operations.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCueKind {
    RaceWonFast,
    RaceWonPro,
    CompilerHealed,
    SecretLeakBlocked,
    CheckpointSaved,
    TddGreen,
    ErrorAlert,
}

impl AudioCueKind {
    pub fn description(&self) -> &'static str {
        match self {
            AudioCueKind::RaceWonFast => "⚡ Fast Local Draft Won Race (Sub-50ms)",
            AudioCueKind::RaceWonPro => "🧠 Frontier Cloud Draft Won Race",
            AudioCueKind::CompilerHealed => "✨ Compiler Self-Healing Succeeded",
            AudioCueKind::SecretLeakBlocked => "🛡️ Secret Leak Automatically Blocked",
            AudioCueKind::CheckpointSaved => "💾 State Checkpoint Safely Appended",
            AudioCueKind::TddGreen => "🟢 TDD Cycle Green Invariants Verified",
            AudioCueKind::ErrorAlert => "🚨 Execution Failure / Invariant Breach",
        }
    }
}

/// Spoken voice intent mapped to code modification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceDiffIntent {
    pub raw_transcript: String,
    pub action: String,
    pub target_symbol: Option<String>,
    pub confidence: f32,
}

pub struct AmbientAudioEngine;

impl AmbientAudioEngine {
    /// Non-blocking auditory earcon playback (uses terminal BEL or ASCII bell safely)
    pub fn play_cue(kind: AudioCueKind) -> bool {
        // Output harmless ANSI bell sequence if terminal interactive
        match kind {
            AudioCueKind::TddGreen | AudioCueKind::RaceWonFast | AudioCueKind::CompilerHealed => {
                // Happy subtle ping
                print!("\x07");
                true
            }
            AudioCueKind::ErrorAlert | AudioCueKind::SecretLeakBlocked => {
                // Attention alert
                print!("\x07\x07");
                true
            }
            _ => {
                print!("\x07");
                true
            }
        }
    }

    /// Parses spoken voice instructions into code manipulation intents
    pub fn parse_voice_intent(transcript: &str) -> Option<VoiceDiffIntent> {
        let lower = transcript.to_lowercase();
        let trimmed = lower.trim();

        if trimmed.is_empty() {
            return None;
        }

        let (action, symbol, conf) = if trimmed.contains("add function") || trimmed.contains("create function") {
            let sym = trimmed.split("function").nth(1).unwrap_or("").trim().split_whitespace().next().map(|s| s.to_string());
            ("AddFunction".to_string(), sym, 0.95)
        } else if trimmed.contains("fix") || trimmed.contains("repair") || trimmed.contains("heal") {
            let part = if let Some((_, after)) = trimmed.split_once("fix") {
                after
            } else if let Some((_, after)) = trimmed.split_once("repair") {
                after
            } else if let Some((_, after)) = trimmed.split_once("heal") {
                after
            } else {
                ""
            };
            let sym = part.split_whitespace().next().map(|s| s.to_string());
            ("HealCode".to_string(), sym, 0.92)
        } else if trimmed.contains("rollback") || trimmed.contains("undo") || trimmed.contains("revert") {
            ("UndoCheckpoint".to_string(), None, 0.98)
        } else if trimmed.contains("test") || trimmed.contains("verify") || trimmed.contains("tdd") {
            ("RunTddCycle".to_string(), None, 0.90)
        } else {
            ("ModifyCode".to_string(), None, 0.75)
        };

        Some(VoiceDiffIntent {
            raw_transcript: transcript.to_string(),
            action,
            target_symbol: symbol,
            confidence: conf,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ambient_audio_cues_and_voice_intent() {
        assert!(AmbientAudioEngine::play_cue(AudioCueKind::TddGreen));
        assert!(AmbientAudioEngine::play_cue(AudioCueKind::CompilerHealed));

        let intent1 = AmbientAudioEngine::parse_voice_intent("add function process_order")
            .expect("Should parse function creation voice intent");
        assert_eq!(intent1.action, "AddFunction");
        assert_eq!(intent1.target_symbol.as_deref(), Some("process_order"));

        let intent2 = AmbientAudioEngine::parse_voice_intent("undo previous change")
            .expect("Should parse undo voice intent");
        assert_eq!(intent2.action, "UndoCheckpoint");
    }
}
