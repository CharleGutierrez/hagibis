//! # Brutal Integration Tests for Hagibis Superpowers 79..83
//!
//! Brutally tests:
//! 1. Superpower 79: SaasMonetizationFabric (Stripe/LemonSqueezy, HMAC signatures, idempotent replay protection, Next.js/Axum scaffolding)
//! 2. Superpower 80: ContinuousVoiceDuplex (VAD thresholding, full-duplex ambient loop, barge-in interruption, acoustic earcons)
//! 3. Superpower 81: FigmaDesignBridge (REST tokens, Tailwind config generation, React component AST synthesis, reverse SVG canvas export)
//! 4. Superpower 82: ShadowDbStressFuzzer (Concurrent synthetic workload fuzzer, p50/p95/p99 latency percentiles, auto SQL index recommendations)
//! 5. Superpower 83: ViralSocialOgEngine (Dynamic 1200x630 SVG OG cards, Next.js edge route, Twitter Card meta tags, JSON-LD schema, viral scorecard)
//! 6. Daemon IPC protocol round-trip serialization and deserialization (Requests 79..83 <-> Responses 79..83)

use hgb_core::protocol::HgbRequest;
use hgb_core::*;

// =========================================================================
// 1. Superpower 79: SaasMonetizationFabric
// =========================================================================
#[test]
fn test_brutal_superpower_79_saas_monetization_fabric() {
    let fabric = SaasMonetizationFabric::new();

    // 1.1 Webhook Verification & Replay Protection
    let stripe_payload = r#"{"id": "evt_charge_9988", "type": "checkout.session.completed", "amount": 2900}"#;
    let secret = "whsec_live_prod_abc";
    let sig = "t=1700000000,v1=3aa54bb4c089550289bddcb229d32fdf6a9d51d4fa5b795b6de446fb00cb9167";

    let verify_first = fabric.verify_webhook(SaasProvider::Stripe, stripe_payload, sig, secret);
    assert!(verify_first.valid, "First webhook verification must succeed");
    assert_eq!(verify_first.event_id, "evt_charge_9988");
    assert_eq!(verify_first.event_type, "checkout.session.completed");
    assert!(!verify_first.is_duplicate, "First event must not be duplicate");

    // Replay attack attempt: same event ID must be rejected as duplicate
    let verify_replay = fabric.verify_webhook(SaasProvider::Stripe, stripe_payload, sig, secret);
    assert!(verify_replay.valid);
    assert!(verify_replay.is_duplicate, "Replay attack must be detected and blocked");
    assert!(verify_replay.error.unwrap().contains("idempotent skip"));

    // 1.2 LemonSqueezy Webhook Verification
    let ls_payload = r#"{"meta": {"event_id": "ls_evt_5544", "event_name": "subscription_created"}, "data": {}}"#;
    let ls_sig = "sha256=ea6971b150cc1f77f9b4d591c259f615632ebe2353b69190ca5612a6f60da30d";
    let ls_verify = fabric.verify_webhook(SaasProvider::LemonSqueezy, ls_payload, ls_sig, secret);
    assert!(ls_verify.valid);
    assert_eq!(ls_verify.event_id, "ls_evt_5544");
    assert_eq!(ls_verify.event_type, "subscription_created");

    // 1.3 Fullstack SaaS Scaffolding (Next.js)
    let config = SaasScaffoldConfig {
        provider: SaasProvider::Stripe,
        project_name: "rapid-saas".into(),
        framework: "nextjs".into(),
        tiers: vec![
            PricingTier {
                name: "Starter".into(),
                price_cents: 900,
                interval: "month".into(),
                stripe_price_id: "price_starter".into(),
                features: vec!["Core Features".into()],
            },
            PricingTier {
                name: "Unlimited".into(),
                price_cents: 4900,
                interval: "month".into(),
                stripe_price_id: "price_unlimited".into(),
                features: vec!["All Features".into(), "Dedicated IP".into()],
            },
        ],
        enable_customer_portal: true,
        enable_jwt_auth: true,
    };

    let report = fabric.scaffold(config);
    assert_eq!(report.project_name, "rapid-saas");
    assert_eq!(report.active_tiers.len(), 2);
    assert!(report.auth_middleware_configured);
    assert!(report.idempotent_guard_enabled);
    assert_eq!(report.generated_files.len(), 3);
    assert!(report.generated_files.iter().any(|f| f.relative_path.contains("route.ts")));
    assert!(report.generated_files.iter().any(|f| f.relative_path.contains("middleware.ts")));
}

// =========================================================================
// 2. Superpower 80: ContinuousVoiceDuplex
// =========================================================================
#[test]
fn test_brutal_superpower_80_continuous_voice_duplex() {
    let mut voice = ContinuousVoiceDuplex::new(ContinuousVoiceConfig {
        vad_threshold: 0.5,
        silence_timeout_ms: 600,
        allow_barge_in: true,
        enable_acoustic_earcons: true,
        preferred_voice_model: "gemini-live".into(),
    });

    assert_eq!(voice.current_state(), VoiceDuplexState::Idle);

    // 2.1 VAD Silence vs Speech detection
    let s_quiet = voice.process_vad_energy(0.2);
    assert_eq!(s_quiet, VoiceDuplexState::Idle);

    let s_speech = voice.process_vad_energy(0.75);
    assert_eq!(s_speech, VoiceDuplexState::UserSpeaking);

    // 2.2 Turn Submission & Earcon mapping
    let user_turn = voice.submit_turn("user", "Make background dark and button neon green", Some("mutate_css".into()));
    assert_eq!(user_turn.speaker, "user");
    assert_eq!(user_turn.earcon_played, Some(AcousticEarcon::IntentUnderstood));
    assert_eq!(voice.current_state(), VoiceDuplexState::ProcessingIntent);

    // 2.3 Model Speaking state
    let model_turn = voice.submit_turn("hgb_agent", "Styles updated successfully.", None);
    assert_eq!(model_turn.earcon_played, Some(AcousticEarcon::DiffAppliedSuccess));
    assert_eq!(voice.current_state(), VoiceDuplexState::ModelSpeaking);

    // 2.4 Barge-in Interruption Detection
    // While model is speaking, user speaks loudly (energy 0.8)
    let s_interrupted = voice.process_vad_energy(0.8);
    assert_eq!(s_interrupted, VoiceDuplexState::Interrupted);

    let report = voice.report();
    assert_eq!(report.total_interruptions, 1);
    assert!(report.total_turns >= 2);
    assert!(report.continuous_mode_active);
}

// =========================================================================
// 3. Superpower 81: FigmaDesignBridge
// =========================================================================
#[test]
fn test_brutal_superpower_81_figma_design_bridge() {
    let bridge = FigmaDesignBridge::new();

    // 3.1 Figma REST Token Ingestion & Component AST Synthesis
    let sync_rep = bridge.sync_tokens_and_components("figma_prod_file_key", None);
    assert_eq!(sync_rep.file_key, "figma_prod_file_key");
    assert!(!sync_rep.token_set.colors.is_empty());
    assert!(!sync_rep.token_set.typography.is_empty());
    assert!(sync_rep.generated_tailwind_config.contains("borderRadius"));
    assert!(sync_rep.synthesized_components.contains_key("VibeHeroBanner"));

    let hero_jsx = sync_rep.synthesized_components.get("VibeHeroBanner").unwrap();
    assert!(hero_jsx.contains("export function VibeHeroBanner()"));
    assert!(hero_jsx.contains("PrimaryCtaButton"));

    // 3.2 Reverse SVG Vector Canvas Export
    let export_rep = bridge.export_to_vector_canvas(
        "PricingWidget",
        "<div class=\"bg-slate-900 border rounded-xl\"><h3>Pro Tier $29</h3></div>",
    );
    assert_eq!(export_rep.component_name, "PricingWidget");
    assert_eq!(export_rep.bounding_width, 640.0);
    assert_eq!(export_rep.bounding_height, 360.0);
    assert!(export_rep.svg_canvas_xml.contains("<svg"));
    assert!(export_rep.svg_canvas_xml.contains("PricingWidget"));
    assert!(export_rep.figma_compatible_json.get("type").is_some());
}

// =========================================================================
// 4. Superpower 82: ShadowDbStressFuzzer
// =========================================================================
#[test]
fn test_brutal_superpower_82_shadow_db_stress_fuzzer() {
    let fuzzer = ShadowDbStressFuzzer::new();
    let profile = StressProfile {
        target_db: "postgres".into(),
        total_operations: 1000,
        concurrency_workers: 16,
        read_write_ratio: 0.9,
        simulated_dataset_size: 10000,
    };

    let schema_sql = "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT);\n\
                      CREATE TABLE orders (id TEXT PRIMARY KEY, user_id TEXT, amount INT);";

    let report = fuzzer.run_stress_test(profile, Some(schema_sql));
    assert_eq!(report.metrics.total_ops, 1000);
    assert!(report.metrics.throughput_qps > 0.0);
    assert!(report.metrics.p50_ms <= report.metrics.p90_ms);
    assert!(report.metrics.p90_ms <= report.metrics.p95_ms);
    assert!(report.metrics.p95_ms <= report.metrics.p99_ms);

    // Bottlenecks & Index recommendation synthesis
    assert!(!report.recommended_indexes.is_empty());
    assert!(report.recommended_indexes.iter().any(|idx| idx.column == "user_id"));
    assert!(report.recommended_indexes.iter().any(|idx| idx.column == "email"));
}

// =========================================================================
// 5. Superpower 83: ViralSocialOgEngine
// =========================================================================
#[test]
fn test_brutal_superpower_83_viral_social_og_engine() {
    let engine = ViralSocialOgEngine::new();
    let config = OgCardConfig {
        title: "Autonomous AI Dev Engine for Speedrun SaaS".into(),
        description: "Zero telemetry, sub-millisecond Rust microkernel, instant Stripe monetization.".into(),
        badge_text: "⚡ 1000x Faster".into(),
        primary_brand_color: "#06b6d4".into(),
        site_url: "https://hagibis.dev".into(),
        author_twitter_handle: "@hagibis_ai".into(),
    };

    let report = engine.generate_viral_suite(config);

    // 5.1 Dynamic SVG OG Card
    assert!(report.generated_svg_image.contains("<svg"));
    assert!(report.generated_svg_image.contains("viewBox=\"0 0 1200 630\""));
    assert!(report.generated_svg_image.contains("Autonomous AI Dev Engine"));

    // 5.2 Next.js Edge Route
    assert!(report.generated_edge_route_code.contains("export const runtime = 'edge';"));
    assert!(report.generated_edge_route_code.contains("ImageResponse"));

    // 5.3 Social Meta Tags & Schema
    assert!(report.meta_tags.iter().any(|t| t.property_or_name == "twitter:card"));
    assert!(report.meta_tags.iter().any(|t| t.property_or_name == "og:title"));
    assert!(report.json_ld_schema.contains("SoftwareApplication"));

    // 5.4 Pre-launch Viral Scorecard
    assert_eq!(report.scorecard.total_score, 100);
    assert!(report.scorecard.title_length_ok);
    assert!(report.scorecard.description_length_ok);
}

// =========================================================================
// 6. IPC Protocol Round-Trip Serialization (Superpowers 79..83)
// =========================================================================
#[test]
fn test_brutal_daemon_ipc_roundtrip_superpowers_79_to_83() {
    // 79. SaasScaffold & Verify
    let req_saas = HgbRequest::SaasScaffold {
        config: SaasScaffoldConfig {
            provider: SaasProvider::Stripe,
            project_name: "test-saas".into(),
            framework: "nextjs".into(),
            tiers: vec![],
            enable_customer_portal: true,
            enable_jwt_auth: true,
        },
    };
    let encoded = bincode::serialize(&req_saas).expect("Failed to serialize SaasScaffold");
    let decoded: HgbRequest = bincode::deserialize(&encoded).expect("Failed to deserialize SaasScaffold");
    assert!(matches!(decoded, HgbRequest::SaasScaffold { .. }));

    // 80. ContinuousVoiceTurn
    let req_voice = HgbRequest::ContinuousVoiceTurn {
        speaker: "user".into(),
        transcript: "Make the button glowing neon cyan".into(),
        intent_action: Some("mutate_css".into()),
        energy: Some(0.85),
    };
    let encoded = bincode::serialize(&req_voice).expect("Failed to serialize ContinuousVoiceTurn");
    let decoded: HgbRequest = bincode::deserialize(&encoded).expect("Failed to deserialize ContinuousVoiceTurn");
    assert!(matches!(decoded, HgbRequest::ContinuousVoiceTurn { .. }));

    // 81. FigmaSync & Export
    let req_figma = HgbRequest::FigmaSync {
        file_key: "key_xyz".into(),
        raw_json: None,
    };
    let encoded = bincode::serialize(&req_figma).expect("Failed to serialize FigmaSync");
    let decoded: HgbRequest = bincode::deserialize(&encoded).expect("Failed to deserialize FigmaSync");
    assert!(matches!(decoded, HgbRequest::FigmaSync { .. }));

    // 82. ShadowDbStress
    let req_db = HgbRequest::ShadowDbStress {
        profile: StressProfile::default(),
        schema_sql: None,
    };
    let encoded = bincode::serialize(&req_db).expect("Failed to serialize ShadowDbStress");
    let decoded: HgbRequest = bincode::deserialize(&encoded).expect("Failed to deserialize ShadowDbStress");
    assert!(matches!(decoded, HgbRequest::ShadowDbStress { .. }));

    // 83. ViralOgGenerate
    let req_og = HgbRequest::ViralOgGenerate {
        config: OgCardConfig::default(),
    };
    let encoded = bincode::serialize(&req_og).expect("Failed to serialize ViralOgGenerate");
    let decoded: HgbRequest = bincode::deserialize(&encoded).expect("Failed to deserialize ViralOgGenerate");
    assert!(matches!(decoded, HgbRequest::ViralOgGenerate { .. }));
}
