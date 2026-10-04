use hgb_core::embedded_firmware_lab::{EmbeddedFirmwareEngine, EmbeddedCheckConfig, TargetMcuArchitecture};
use hgb_core::speech_synthesis_engine::{SpeechSynthesisEngine, SynthesisConfig};

#[test]
fn test_embedded_firmware_engine_brutal() {
    let config = EmbeddedCheckConfig {
        target_arch: TargetMcuArchitecture::RiscV,
        no_std: false,
        max_flash_bytes: 1024 * 1024,
        max_ram_bytes: 256 * 1024,
    };

    let snippet = r#"
        #![no_std]
        
        struct Config {
            a: u32,
            b: u32,
        }
        
        static mut GLOBAL_STATE: u32 = 0;
        
        fn compute() -> u32 {
            let x = 1 + 2;
            x
        }
        
        const MAX_VAL: u32 = 100;
        
        enum State {
            Init,
            Running,
            Error,
        }
    "#;

    let report = EmbeddedFirmwareEngine::check_firmware(snippet, &config).unwrap();
    
    assert!(report.compiles_no_std, "Should detect #![no_std]");
    assert!(report.estimated_flash_bytes > 0, "Should have positive flash usage");
    assert!(report.estimated_ram_bytes > 0, "Should have positive ram usage");
    assert!(report.memory_invariants_passed, "Should pass memory invariants");
}

#[test]
fn test_speech_synthesis_engine_brutal() {
    let config = SynthesisConfig {
        voice_id: "en_US-kokoro-v1".to_string(),
        speaking_rate: 1.2,
        pitch: 1.1,
        output_format: "wav".to_string(),
    };

    let text = "Hello world, this is a brutal test of the TTS engine.";
    
    // This will either use tts or gracefully fallback
    let report = SpeechSynthesisEngine::synthesize(text, &config).unwrap();
    
    assert_eq!(report.voice_used, "en_US-kokoro-v1");
    assert!(report.audio_bytes_len > 0, "Audio bytes should not be empty");
    assert!(!report.phonemes.is_empty(), "Phonemes should be extracted");
}
