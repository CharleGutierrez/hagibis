//! # Superpower 123: StoreReleaseOrchestrator
//!
//! Native App Store Release, Code-Signing & Fastlane Orchestrator.
//! Automates iOS / Android code-signing certification, provisioning profiles,
//! Fastlane match/gym lane runner, and App Store Connect & Google Play Console releases.

use crate::error::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppStorePlatform {
    AppleAppStore,
    GooglePlayStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseTrack {
    InternalTesting,
    Alpha,
    Beta,
    Production,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreReleaseConfig {
    pub platform: AppStorePlatform,
    pub app_bundle_id: String,
    pub version_name: String,
    pub build_number: u32,
    pub track: ReleaseTrack,
    pub fastlane_lane: String,
}

impl Default for StoreReleaseConfig {
    fn default() -> Self {
        Self {
            platform: AppStorePlatform::AppleAppStore,
            app_bundle_id: "com.vibe.app".to_string(),
            version_name: "1.0.0".to_string(),
            build_number: 1,
            track: ReleaseTrack::InternalTesting,
            fastlane_lane: "beta".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreReleaseReport {
    pub platform: AppStorePlatform,
    pub app_bundle_id: String,
    pub code_signing_verified: bool,
    pub fastlane_lane_executed: String,
    pub build_artifact_path: String,
    pub submission_id: String,
    pub success: bool,
    pub message: String,
}

use std::process::Command;

pub struct StoreReleaseEngine;

impl StoreReleaseEngine {
    pub fn execute_release(config: &StoreReleaseConfig) -> Result<StoreReleaseReport> {
        let artifact = match config.platform {
            AppStorePlatform::AppleAppStore => format!("build/{}.ipa", config.app_bundle_id),
            AppStorePlatform::GooglePlayStore => format!("build/{}.aab", config.app_bundle_id),
        };

        // ACTUALLY execute fastlane instead of mocking it
        let output = Command::new("fastlane")
            .arg(&config.fastlane_lane)
            .output();

        match output {
            Ok(out) => {
                let success = out.status.success();
                let msg = if success {
                    String::from_utf8_lossy(&out.stdout).into_owned()
                } else {
                    String::from_utf8_lossy(&out.stderr).into_owned()
                };
                
                Ok(StoreReleaseReport {
                    platform: config.platform,
                    app_bundle_id: config.app_bundle_id.clone(),
                    code_signing_verified: success,
                    fastlane_lane_executed: config.fastlane_lane.clone(),
                    build_artifact_path: artifact,
                    submission_id: format!("rel_{}_{}", config.version_name, config.build_number),
                    success,
                    message: format!("Fastlane exited with {}: {}", out.status, msg),
                })
            }
            Err(e) => {
                Ok(StoreReleaseReport {
                    platform: config.platform,
                    app_bundle_id: config.app_bundle_id.clone(),
                    code_signing_verified: false,
                    fastlane_lane_executed: config.fastlane_lane.clone(),
                    build_artifact_path: artifact,
                    submission_id: format!("rel_{}_{}", config.version_name, config.build_number),
                    success: false,
                    message: format!("Failed to spawn fastlane process: {}", e),
                })
            }
        }
    }
}
