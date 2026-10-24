//! Ambient Audio Cues ("Sound of Green")
//!
//! Provides non-blocking acoustic cues and terminal bell frequencies for Vibe Coding.
//! On success ("Sound of Green"): pleasant acoustic signal / terminal chime (\x07).
//! On failure: low alert frequency cue (\x07\x07).

use std::io::{self, Write};
use serde::{Deserialize, Serialize};

/// High-level ambient audio cues across compilation and self-healing lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VibeAudioCue {
    /// Upward chirp signaling compilation/build initiation
    CompileStart,
    /// Dual harmonic pulse signaling autonomous healing in progress
    AutoHealing,
    /// Ascending major chord ("Sound of Green") on verified success
    GreenSuccess,
    /// Double alert pulse when user approval or intervention is required
    ApprovalAlert,
}

/// Standardized tactile chime cues specifically engineered for Vibe Coding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibeChimeKind {
    /// "Sound of Green": Ascending major triad chord on passing tests / verified build
    Green,
    /// Low discordant alert tone on invariant breach or failure
    Error,
    /// Dual harmonic resonance pulse when autonomous compiler/test healing succeeds
    Heal,
    /// Ultra-fast pitch sweep chirp when frontier/local speculative race completes
    Race,
}

/// Synthesizes high-fidelity 16-bit PCM waveform samples (mono) for the requested chime cue.
pub fn synthesize_vibe_chime_pcm(kind: VibeChimeKind, sample_rate: u32) -> Vec<i16> {
    let sample_rate = sample_rate.max(8000);
    let mut samples = Vec::new();

    match kind {
        VibeChimeKind::Green => {
            // Ascending major chord C5 (523.25Hz), E5 (659.25Hz), G5 (783.99Hz)
            let duration_secs = 0.22;
            let total_samples = (duration_secs * sample_rate as f32) as usize;
            let freqs = [523.25f32, 659.25, 783.99];

            for i in 0..total_samples {
                let t = i as f32 / sample_rate as f32;
                // Triangular/ADSR envelope
                let env = if t < 0.03 {
                    t / 0.03
                } else {
                    ((duration_secs - t) / (duration_secs - 0.03)).max(0.0)
                };

                let mut val = 0.0;
                for (idx, &f) in freqs.iter().enumerate() {
                    let offset = idx as f32 * 0.04;
                    if t >= offset {
                        let sub_t = t - offset;
                        val += (sub_t * f * 2.0 * std::f32::consts::PI).sin() * 0.33;
                    }
                }
                let pcm_val = (val * env * 22000.0).clamp(-32767.0, 32767.0) as i16;
                samples.push(pcm_val);
            }
        }
        VibeChimeKind::Error => {
            // Low descending discordant buzz (220Hz -> 140Hz)
            let duration_secs = 0.25;
            let total_samples = (duration_secs * sample_rate as f32) as usize;

            for i in 0..total_samples {
                let t = i as f32 / sample_rate as f32;
                let env = (1.0 - t / duration_secs).powi(2);
                let current_freq = 220.0 - (80.0 * (t / duration_secs));
                let harmonic = (current_freq * 1.5).round();

                let val = (t * current_freq * 2.0 * std::f32::consts::PI).sin() * 0.6
                    + (t * harmonic * 2.0 * std::f32::consts::PI).sin() * 0.4;
                let pcm_val = (val * env * 24000.0).clamp(-32767.0, 32767.0) as i16;
                samples.push(pcm_val);
            }
        }
        VibeChimeKind::Heal => {
            // Dual harmonic resonance pulse (440Hz & 880Hz shimmer)
            let duration_secs = 0.20;
            let total_samples = (duration_secs * sample_rate as f32) as usize;

            for i in 0..total_samples {
                let t = i as f32 / sample_rate as f32;
                let env = (-(t - 0.05).powi(2) * 80.0).exp();
                let f1 = 440.0;
                let f2 = 880.0;
                let val = (t * f1 * 2.0 * std::f32::consts::PI).sin() * 0.6
                    + (t * f2 * 2.0 * std::f32::consts::PI).sin() * 0.4;
                let pcm_val = (val * env * 20000.0).clamp(-32767.0, 32767.0) as i16;
                samples.push(pcm_val);
            }
        }
        VibeChimeKind::Race => {
            // Ultra-fast ascending speedrun chirp (880Hz -> 1760Hz sweep in 70ms)
            let duration_secs = 0.07;
            let total_samples = (duration_secs * sample_rate as f32) as usize;

            for i in 0..total_samples {
                let t = i as f32 / sample_rate as f32;
                let env = (1.0 - t / duration_secs);
                let freq = 880.0 + (880.0 * (t / duration_secs));
                let val = (t * freq * 2.0 * std::f32::consts::PI).sin();
                let pcm_val = (val * env * 22000.0).clamp(-32767.0, 32767.0) as i16;
                samples.push(pcm_val);
            }
        }
    }

    samples
}

/// Dispatch a specific chime cue with non-blocking terminal bell representation
pub fn play_vibe_chime_kind(kind: VibeChimeKind) {
    if std::env::var("HGB_AUDIO_DISABLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return;
    }

    let mut stdout = io::stdout();
    match kind {
        VibeChimeKind::Green => {
            let _ = stdout.write_all(b"\x07");
        }
        VibeChimeKind::Error => {
            let _ = stdout.write_all(b"\x07\x07");
        }
        VibeChimeKind::Heal => {
            let _ = stdout.write_all(b"\x07");
        }
        VibeChimeKind::Race => {
            let _ = stdout.write_all(b"\x07");
        }
    }
    let _ = stdout.flush();
}

/// Dispatch a synthesized non-blocking acoustic cue
pub fn play_vibe_cue(cue: VibeAudioCue) {
    match cue {
        VibeAudioCue::CompileStart => play_vibe_chime_kind(VibeChimeKind::Race),
        VibeAudioCue::AutoHealing => play_vibe_chime_kind(VibeChimeKind::Heal),
        VibeAudioCue::GreenSuccess => play_vibe_chime_kind(VibeChimeKind::Green),
        VibeAudioCue::ApprovalAlert => play_vibe_chime_kind(VibeChimeKind::Error),
    }
}

/// Output a non-blocking terminal chime or acoustic cue.
///
/// - `success == true`: "Sound of Green" - pleasant non-blocking acoustic chime.
/// - `success == false`: low alert tone signaling test or check failure.
///
/// This function never panics and safely ignores broken pipes / non-terminal redirections.
/// Honors `HGB_AUDIO_DISABLE=1` to silence cues in headless or restricted CI environments.
pub fn play_vibe_chime(success: bool) {
    if success {
        play_vibe_chime_kind(VibeChimeKind::Green);
    } else {
        play_vibe_chime_kind(VibeChimeKind::Error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_play_vibe_chime_safe_execution() {
        play_vibe_chime(true);
        play_vibe_chime(false);
    }

    #[test]
    fn test_play_vibe_cue_variants() {
        play_vibe_cue(VibeAudioCue::CompileStart);
        play_vibe_cue(VibeAudioCue::AutoHealing);
        play_vibe_cue(VibeAudioCue::GreenSuccess);
        play_vibe_cue(VibeAudioCue::ApprovalAlert);
    }

    #[test]
    fn test_vibe_chime_synthesizers_green_error_heal_race() {
        let sample_rate = 16000;
        
        let pcm_green = synthesize_vibe_chime_pcm(VibeChimeKind::Green, sample_rate);
        assert!(!pcm_green.is_empty());
        assert!(pcm_green.len() > 1000);
        assert!(pcm_green.iter().any(|&s| s != 0));

        let pcm_error = synthesize_vibe_chime_pcm(VibeChimeKind::Error, sample_rate);
        assert!(!pcm_error.is_empty());
        assert!(pcm_error.iter().any(|&s| s != 0));

        let pcm_heal = synthesize_vibe_chime_pcm(VibeChimeKind::Heal, sample_rate);
        assert!(!pcm_heal.is_empty());
        assert!(pcm_heal.iter().any(|&s| s != 0));

        let pcm_race = synthesize_vibe_chime_pcm(VibeChimeKind::Race, sample_rate);
        assert!(!pcm_race.is_empty());
        assert!(pcm_race.iter().any(|&s| s != 0));
    }

    #[test]
    fn test_play_vibe_chime_kind_all_variants() {
        play_vibe_chime_kind(VibeChimeKind::Green);
        play_vibe_chime_kind(VibeChimeKind::Error);
        play_vibe_chime_kind(VibeChimeKind::Heal);
        play_vibe_chime_kind(VibeChimeKind::Race);
    }
}
