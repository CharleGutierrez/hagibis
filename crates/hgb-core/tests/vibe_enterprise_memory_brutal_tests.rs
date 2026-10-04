use hgb_core::enterprise_gateway::{EnterpriseGateway, EnterpriseCompliance, Claims};
use hgb_core::colibri::{MultiTierMemory, MemoryTieringEngine};
use jsonwebtoken::{encode, Header, EncodingKey};

#[test]
fn test_enterprise_gateway_auth() {
    let secret = b"super_secret_test_key";
    std::env::set_var("HGB_JWT_SECRET", "super_secret_test_key");
    
    let gateway = EnterpriseGateway::new();
    
    let config = EnterpriseCompliance {
        sso_provider: "okta".to_string(),
        require_mfa: true,
        audit_logging: true,
    };
    
    let claims = Claims {
        sub: "test_user_99".to_string(),
        exp: 20000000000,
        mfa: Some(true),
        audit: Some(true),
    };
    
    let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(secret)).unwrap();
    
    let result = gateway.authenticate(&config, &token);
    assert!(result.authorized);
    assert_eq!(result.user_id, "test_user_99");
    assert_eq!(result.compliance_score, 100);
}

#[test]
fn test_colibri_memory_tiering() {
    let nvme_path = "/tmp/hgb_test_swap_brutal.bin";
    let memory = MultiTierMemory::new(nvme_path, 2, 1);
    
    let result = memory.stream_weights("test_model");
    assert!(result.is_ok());
    
    let metadata = std::fs::metadata(nvme_path).unwrap();
    assert_eq!(metadata.len(), 2 * 1024 * 1024);
    
    // Cleanup
    let _ = std::fs::remove_file(nvme_path);
}
