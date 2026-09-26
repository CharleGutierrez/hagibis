//! Ambient Audio Cues ("Sound of Green")
//!
//! Provides non-blocking acoustic cues and terminal bell frequencies for Vibe Coding.
//! On success ("Sound of Green"): pleasant acoustic signal / terminal chime (\x07).
//! On failure: low alert frequency cue (\x07\x07).

use std::io::{self, Write};

/// Output a non-blocking terminal chime or acoustic cue.
///
/// - `success == true`: "Sound of Green" - pleasant non-blocking acoustic chime.
/// - `success == false`: low alert tone signaling test or check failure.
///
/// This function never panics and safely ignores broken pipes / non-terminal redirections.
/// Honors `HGB_AUDIO_DISABLE=1` to silence cues in headless or restricted CI environments.
pub fn play_vibe_chime(success: bool) {
    if std::env::var("HGB_AUDIO_DISABLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return;
    }

    let mut stdout = io::stdout();
    if success {
        // High bright ascending green chime cue: standard ASCII bell
        let _ = stdout.write_all(b"\x07");
    } else {
        // Double alert tone for failures
        let _ = stdout.write_all(b"\x07\x07");
    }
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_play_vibe_chime_safe_execution() {
        // Must execute without panic on both true and false
        play_vibe_chime(true);
        play_vibe_chime(false);
    }
}
