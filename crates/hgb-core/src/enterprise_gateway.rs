use serde::{Deserialize, Serialize};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnterpriseCompliance {
    pub sso_provider: String,
    pub require_mfa: bool,
    pub audit_logging: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayAuthResult {
    pub authorized: bool,
    pub user_id: String,
    pub compliance_score: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    pub mfa: Option<bool>,
    pub audit: Option<bool>,
}

pub struct EnterpriseGateway {
    decoding_key: DecodingKey,
}

impl EnterpriseGateway {
    pub fn new() -> Self {
        let secret = std::env::var("HGB_JWT_SECRET").unwrap_or_else(|_| "default_enterprise_secret".to_string());
        Self {
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    pub fn authenticate(&self, config: &EnterpriseCompliance, token: &str) -> GatewayAuthResult {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp", "sub"]);
        
        let mut authorized = false;
        let mut user_id = "anonymous".to_string();
        let mut compliance_score = 0;

        if let Ok(token_data) = decode::<Claims>(token, &self.decoding_key, &validation) {
            authorized = true;
            user_id = token_data.claims.sub.clone();
            
            let mut score = 50;
            if config.require_mfa {
                if token_data.claims.mfa.unwrap_or(false) {
                    score += 25;
                }
            } else {
                score += 25;
            }
            if config.audit_logging {
                if token_data.claims.audit.unwrap_or(false) {
                    score += 25;
                }
            } else {
                score += 25;
            }
            compliance_score = score;
        } else if !config.require_mfa && !config.audit_logging {
            // fallback for legacy or minimal compliance if needed, but we fail auth
        }

        GatewayAuthResult {
            authorized,
            user_id,
            compliance_score,
        }
    }
}
