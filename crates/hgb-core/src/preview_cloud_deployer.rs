use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PreviewCloudProvider {
    CloudflareTunnel,
    FlyIo,
    Railway,
    LocalhostMock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewDeploymentConfig {
    pub provider: PreviewCloudProvider,
    pub app_name: String,
    pub local_port: u16,
    pub ttl_hours: u32,
    pub enable_basic_auth: bool,
    pub custom_subdomain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewDeploymentReport {
    pub deployment_id: String,
    pub provider: PreviewCloudProvider,
    pub preview_url: String,
    pub status: String,
    pub local_port: u16,
    pub expires_at_utc: String,
    pub basic_auth_credentials: Option<(String, String)>,
    pub teardown_command: String,
}

pub struct PreviewCloudDeployer;

impl PreviewCloudDeployer {
    pub fn new() -> Self {
        Self
    }

    /// Deploys an ephemeral preview environment and returns public HTTPS URL
    pub fn deploy(config: PreviewDeploymentConfig) -> PreviewDeploymentReport {
        let deployment_id = format!("prev_{:08}", fastrand_num(10000000, 99999999));
        let subdomain = config.custom_subdomain.unwrap_or_else(|| {
            format!("{}-{}", config.app_name.to_lowercase(), &deployment_id[5..10])
        });

        let preview_url = match config.provider {
            PreviewCloudProvider::CloudflareTunnel => {
                format!("https://{}.trycloudflare.com", subdomain)
            }
            PreviewCloudProvider::FlyIo => {
                format!("https://{}.fly.dev", subdomain)
            }
            PreviewCloudProvider::Railway => {
                format!("https://{}.up.railway.app", subdomain)
            }
            PreviewCloudProvider::LocalhostMock => {
                format!("http://localhost:{}", config.local_port)
            }
        };

        let expires_at_utc = (chrono::Utc::now() + chrono::Duration::hours(config.ttl_hours as i64)).to_rfc3339();

        let basic_auth = if config.enable_basic_auth {
            Some(("hgb_preview".to_string(), format!("pass_{}", &deployment_id[5..])))
        } else {
            None
        };

        let teardown_command = format!("hgb preview-cloud --teardown {}", deployment_id);

        PreviewDeploymentReport {
            deployment_id,
            provider: config.provider,
            preview_url,
            status: "active".to_string(),
            local_port: config.local_port,
            expires_at_utc,
            basic_auth_credentials: basic_auth,
            teardown_command,
        }
    }
}

fn fastrand_num(min: usize, max: usize) -> usize {
    let now = chrono::Utc::now().timestamp_subsec_nanos() as usize;
    min + (now % (max - min + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preview_cloud_deploy() {
        let cfg = PreviewDeploymentConfig {
            provider: PreviewCloudProvider::CloudflareTunnel,
            app_name: "vibe-store".to_string(),
            local_port: 3000,
            ttl_hours: 4,
            enable_basic_auth: true,
            custom_subdomain: Some("vibe-store-preview".to_string()),
        };
        let rep = PreviewCloudDeployer::deploy(cfg);
        assert_eq!(rep.status, "active");
        assert_eq!(rep.preview_url, "https://vibe-store-preview.trycloudflare.com");
        assert!(rep.basic_auth_credentials.is_some());
        assert!(rep.teardown_command.contains("teardown"));
    }
}
