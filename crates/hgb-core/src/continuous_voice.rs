//! Superpower 80: Full-Duplex Ambient Conversational Voice Loop (hgb voice --continuous)
//!
//! Provides a real-time, low-latency, ambient bidirectional voice streaming loop:
//! - Voice Activity Detection (VAD) energy thresholding & silence detection
//! - Low-latency interruption / barge-in handling (pauses model immediately when user speaks)
//! - Acoustic earcon soundscape cues for ambient tactile feedback
//! - Turn-by-turn conversational state machine synchronized with the live code canvas

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceDuplexState {
    Idle,
    Listening,
    UserSpeaking,
    ProcessingIntent,
    ModelSpeaking,
    Interrupted,
}

impl Default for VoiceDuplexState {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcousticEarcon {
    WakeWordDetected,
    IntentUnderstood,
    DiffAppliedSuccess,
    CompilationFailed,
    RollbackExecuted,
    BargeInPaused,
    SessionStarted,
    SessionTerminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTurnEvent {
    pub timestamp_ms: u64,
    pub speaker: String, // "user" | "hgb_agent"
    pub transcript: String,
    pub interrupted: bool,
    pub earcon_played: Option<AcousticEarcon>,
    pub intent_action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuousVoiceConfig {
    pub vad_threshold: f32, // 0.0 .. 1.0 (default 0.45)
    pub silence_timeout_ms: u64, // e.g. 700ms
    pub allow_barge_in: bool,
    pub enable_acoustic_earcons: bool,
    pub preferred_voice_model: String, // "local-whisper", "gemini-live"
}

impl Default for ContinuousVoiceConfig {
    fn default() -> Self {
        Self {
            vad_threshold: 0.45,
            silence_timeout_ms: 700,
            allow_barge_in: true,
            enable_acoustic_earcons: true,
            preferred_voice_model: "gemini-live".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSessionReport {
    pub session_id: String,
    pub state: VoiceDuplexState,
    pub total_turns: usize,
    pub total_interruptions: usize,
    pub turns: Vec<VoiceTurnEvent>,
    pub active_earcons: Vec<AcousticEarcon>,
    pub continuous_mode_active: bool,
}

#[derive(Debug, Clone)]
pub struct ContinuousVoiceDuplex {
    config: ContinuousVoiceConfig,
    state: VoiceDuplexState,
    session_id: String,
    turns: Vec<VoiceTurnEvent>,
    interruption_count: usize,
}

impl Default for ContinuousVoiceDuplex {
    fn default() -> Self {
        Self::new(ContinuousVoiceConfig::default())
    }
}

impl ContinuousVoiceDuplex {
    pub fn global() -> &'static std::sync::Mutex<Self> {
        static INSTANCE: std::sync::OnceLock<std::sync::Mutex<ContinuousVoiceDuplex>> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(|| std::sync::Mutex::new(ContinuousVoiceDuplex::default()))
    }

    pub fn new(config: ContinuousVoiceConfig) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            config,
            state: VoiceDuplexState::Idle,
            session_id: format!("voice_session_{}", ts),
            turns: Vec::new(),
            interruption_count: 0,
        }
    }

    pub fn current_state(&self) -> VoiceDuplexState {
        self.state
    }

    /// Processes an incoming audio chunk / VAD signal.
    pub fn process_vad_energy(&mut self, energy_level: f32) -> VoiceDuplexState {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if energy_level >= self.config.vad_threshold {
            if self.state == VoiceDuplexState::ModelSpeaking && self.config.allow_barge_in {
                // User interrupted the model! Barge-in trigger!
                self.state = VoiceDuplexState::Interrupted;
                self.interruption_count += 1;
                self.turns.push(VoiceTurnEvent {
                    timestamp_ms: now_ms,
                    speaker: "user".into(),
                    transcript: "[BARGE_IN_DETECTED]".into(),
                    interrupted: true,
                    earcon_played: if self.config.enable_acoustic_earcons {
                        Some(AcousticEarcon::BargeInPaused)
                    } else {
                        None
                    },
                    intent_action: Some("pause_playback".into()),
                });
            } else if self.state != VoiceDuplexState::UserSpeaking {
                self.state = VoiceDuplexState::UserSpeaking;
            }
        } else if self.state == VoiceDuplexState::UserSpeaking {
            self.state = VoiceDuplexState::Listening;
        }

        self.state
    }

    /// Handles a spoken transcript turn from either the user or the agent.
    pub fn submit_turn(
        &mut self,
        speaker: &str,
        transcript: &str,
        intent_action: Option<String>,
    ) -> VoiceTurnEvent {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let earcon = if self.config.enable_acoustic_earcons {
            match speaker {
                "user" => Some(AcousticEarcon::IntentUnderstood),
                _ => {
                    if transcript.contains("error") || transcript.contains("failed") {
                        Some(AcousticEarcon::CompilationFailed)
                    } else if transcript.contains("applied") || transcript.contains("updated") {
                        Some(AcousticEarcon::DiffAppliedSuccess)
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        };

        if speaker == "user" {
            self.state = VoiceDuplexState::ProcessingIntent;
        } else {
            self.state = VoiceDuplexState::ModelSpeaking;
        }

        let event = VoiceTurnEvent {
            timestamp_ms: now_ms,
            speaker: speaker.to_string(),
            transcript: transcript.to_string(),
            interrupted: false,
            earcon_played: earcon,
            intent_action,
        };

        self.turns.push(event.clone());
        event
    }

    /// Generates the complete real-time session report.
    pub fn report(&self) -> VoiceSessionReport {
        let earcons = self
            .turns
            .iter()
            .filter_map(|t| t.earcon_played)
            .collect();

        VoiceSessionReport {
            session_id: self.session_id.clone(),
            state: self.state,
            total_turns: self.turns.len(),
            total_interruptions: self.interruption_count,
            turns: self.turns.clone(),
            active_earcons: earcons,
            continuous_mode_active: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_continuous_voice_duplex_lifecycle_and_barge_in() {
        let mut duplex = ContinuousVoiceDuplex::new(ContinuousVoiceConfig {
            vad_threshold: 0.4,
            silence_timeout_ms: 500,
            allow_barge_in: true,
            enable_acoustic_earcons: true,
            preferred_voice_model: "gemini-live".into(),
        });

        assert_eq!(duplex.current_state(), VoiceDuplexState::Idle);

        // User speaks
        let s = duplex.process_vad_energy(0.65);
        assert_eq!(s, VoiceDuplexState::UserSpeaking);

        // Turn submitted
        let event = duplex.submit_turn("user", "Make the hero button neon cyan", Some("mutate_css".into()));
        assert_eq!(event.earcon_played, Some(AcousticEarcon::IntentUnderstood));

        // Model starts speaking back
        duplex.submit_turn("hgb_agent", "Applying neon cyan styling to hero button now.", None);
        assert_eq!(duplex.current_state(), VoiceDuplexState::ModelSpeaking);

        // User interrupts model (barge-in)
        let s2 = duplex.process_vad_energy(0.7);
        assert_eq!(s2, VoiceDuplexState::Interrupted);

        let report = duplex.report();
        assert_eq!(report.total_interruptions, 1);
        assert!(report.turns.iter().any(|t| t.interrupted));
    }
}
