use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageEcosystem {
    CratesIo,
    Npm,
    PyPi,
}

impl PackageEcosystem {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CratesIo => "crates.io",
            Self::Npm => "npm",
            Self::PyPi => "pypi",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "npm" | "node" | "js" | "ts" | "javascript" | "typescript" | "npmjs" => Self::Npm,
            "pypi" | "python" | "pip" | "py" => Self::PyPi,
            _ => Self::CratesIo,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageStatus {
    Valid,
    NotFound,
    Yanked,
    Deprecated,
    RegistryUnavailable,
    SuspiciousHallucination,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageVerificationReport {
    pub ecosystem: PackageEcosystem,
    pub name: String,
    pub requested_version: Option<String>,
    pub latest_version: Option<String>,
    pub status: PackageStatus,
    pub is_hallucinated: bool,
    pub is_deprecated: bool,
    pub is_yanked: bool,
    pub warning_message: Option<String>,
    pub known_alternatives: Vec<String>,
    pub cached: bool,
    pub verification_time_ms: u64,
}

/// Dependency Hallucination Firewall & Registry Gate
#[derive(Clone)]
pub struct PackageGuard {
    client: reqwest::Client,
    cache: Arc<RwLock<HashMap<String, (PackageVerificationReport, Instant)>>>,
    ttl: Duration,
}

impl Default for PackageGuard {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_millis(750))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl: Duration::from_secs(3600),
        }
    }
}

impl PackageGuard {
    pub fn new(connect_timeout_ms: u64, request_timeout_ms: u64, ttl_secs: u64) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(connect_timeout_ms))
            .timeout(Duration::from_millis(request_timeout_ms))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Check if package name pattern strongly exhibits LLM hallucination markers
    pub fn is_heuristic_hallucination(name: &str, _ecosystem: PackageEcosystem) -> bool {
        let lower = name.to_lowercase();
        let suspicious_suffixes = [
            "-official-rust",
            "-lib-official",
            "-official-sdk",
            "-compat-0.2",
            "-v2-async",
            "-real-node",
            "-ultimate",
        ];
        for s in &suspicious_suffixes {
            if lower.ends_with(s) {
                return true;
            }
        }
        let known_hallucinations = [
            "tokio-curl",
            "tokio-compat-0.2",
            "requests-async-v2",
            "express-auth-ultimate",
            "pandas-ai-native",
            "axum-sqlx-helper",
        ];
        if known_hallucinations.contains(&lower.as_str()) {
            return true;
        }
        false
    }

    /// Suggest real-world alternatives for common hallucinated package names
    pub fn get_suggested_alternatives(name: &str, eco: PackageEcosystem) -> Vec<String> {
        let lower = name.to_lowercase();
        match eco {
            PackageEcosystem::CratesIo => {
                if lower.contains("curl") {
                    vec!["reqwest".to_string(), "curl".to_string()]
                } else if lower.contains("compat") || lower.contains("tokio") {
                    vec!["tokio".to_string(), "tokio-util".to_string()]
                } else {
                    vec![]
                }
            }
            PackageEcosystem::Npm => {
                if lower.contains("auth") {
                    vec!["passport".to_string(), "@auth/core".to_string()]
                } else {
                    vec![]
                }
            }
            PackageEcosystem::PyPi => {
                if lower.contains("request") {
                    vec!["httpx".to_string(), "requests".to_string(), "aiohttp".to_string()]
                } else {
                    vec![]
                }
            }
        }
    }

    /// Verify package against live ecosystem registry with fast timeouts and caching
    pub async fn verify_package(
        &self,
        ecosystem: PackageEcosystem,
        name: &str,
        requested_version: Option<&str>,
    ) -> PackageVerificationReport {
        let start = Instant::now();
        let cache_key = format!("{}:{}:{}", ecosystem.as_str(), name, requested_version.unwrap_or(""));

        // 1. In-memory TTL cache lookup
        {
            let cache_read = self.cache.read().await;
            if let Some((rep, inserted_at)) = cache_read.get(&cache_key) {
                if inserted_at.elapsed() < self.ttl {
                    let mut cached_rep = rep.clone();
                    cached_rep.cached = true;
                    cached_rep.verification_time_ms = start.elapsed().as_millis() as u64;
                    return cached_rep;
                }
            }
        }

        // 2. Pre-flight heuristic check
        if Self::is_heuristic_hallucination(name, ecosystem) {
            let alt = Self::get_suggested_alternatives(name, ecosystem);
            let rep = PackageVerificationReport {
                ecosystem,
                name: name.to_string(),
                requested_version: requested_version.map(|v| v.to_string()),
                latest_version: None,
                status: PackageStatus::SuspiciousHallucination,
                is_hallucinated: true,
                is_deprecated: false,
                is_yanked: false,
                warning_message: Some(format!(
                    "Pattern matches known AI hallucination heuristic for package '{}'",
                    name
                )),
                known_alternatives: alt,
                cached: false,
                verification_time_ms: start.elapsed().as_millis() as u64,
            };
            let mut cache_write = self.cache.write().await;
            cache_write.insert(cache_key, (rep.clone(), Instant::now()));
            return rep;
        }

        // 3. Live registry lookup
        let rep = match ecosystem {
            PackageEcosystem::CratesIo => self.query_crates_io(name, requested_version, start).await,
            PackageEcosystem::Npm => self.query_npm(name, requested_version, start).await,
            PackageEcosystem::PyPi => self.query_pypi(name, requested_version, start).await,
        };

        // Cache report
        let mut cache_write = self.cache.write().await;
        cache_write.insert(cache_key, (rep.clone(), Instant::now()));
        rep
    }

    async fn query_crates_io(
        &self,
        name: &str,
        requested_version: Option<&str>,
        start: Instant,
    ) -> PackageVerificationReport {
        let url = format!("https://crates.io/api/v1/crates/{}", name);
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("hagibis-package-guard/0.1.0"));

        let res = self.client.get(&url).headers(headers).send().await;
        match res {
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                let alt = Self::get_suggested_alternatives(name, PackageEcosystem::CratesIo);
                PackageVerificationReport {
                    ecosystem: PackageEcosystem::CratesIo,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: None,
                    status: PackageStatus::NotFound,
                    is_hallucinated: true,
                    is_deprecated: false,
                    is_yanked: false,
                    warning_message: Some(format!("Crate '{}' does not exist on crates.io", name)),
                    known_alternatives: alt,
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
            Ok(resp) if resp.status().is_success() => {
                let json: serde_json::Value = resp.json().await.unwrap_or_default();
                let latest = json.pointer("/crate/max_version")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                
                let mut is_yanked = false;
                if let Some(req_ver) = requested_version {
                    if let Some(versions) = json.pointer("/versions").and_then(|v| v.as_array()) {
                        for ver in versions {
                            if ver.get("num").and_then(|n| n.as_str()) == Some(req_ver) {
                                if ver.get("yanked").and_then(|y| y.as_bool()) == Some(true) {
                                    is_yanked = true;
                                }
                                break;
                            }
                        }
                    }
                }

                let status = if is_yanked {
                    PackageStatus::Yanked
                } else {
                    PackageStatus::Valid
                };

                PackageVerificationReport {
                    ecosystem: PackageEcosystem::CratesIo,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: latest,
                    status,
                    is_hallucinated: false,
                    is_deprecated: false,
                    is_yanked,
                    warning_message: if is_yanked {
                        Some(format!("Version {} of crate '{}' is yanked", requested_version.unwrap_or(""), name))
                    } else {
                        None
                    },
                    known_alternatives: vec![],
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
            _ => {
                // Offline fallback or network timeout
                PackageVerificationReport {
                    ecosystem: PackageEcosystem::CratesIo,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: None,
                    status: PackageStatus::RegistryUnavailable,
                    is_hallucinated: false,
                    is_deprecated: false,
                    is_yanked: false,
                    warning_message: Some("Registry lookup timed out or network unavailable; skipping enforcement".to_string()),
                    known_alternatives: vec![],
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
        }
    }

    async fn query_npm(
        &self,
        name: &str,
        requested_version: Option<&str>,
        start: Instant,
    ) -> PackageVerificationReport {
        let url = format!("https://registry.npmjs.org/{}", name);
        let res = self.client.get(&url).send().await;
        match res {
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                let alt = Self::get_suggested_alternatives(name, PackageEcosystem::Npm);
                PackageVerificationReport {
                    ecosystem: PackageEcosystem::Npm,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: None,
                    status: PackageStatus::NotFound,
                    is_hallucinated: true,
                    is_deprecated: false,
                    is_yanked: false,
                    warning_message: Some(format!("Package '{}' not found in npm registry", name)),
                    known_alternatives: alt,
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
            Ok(resp) if resp.status().is_success() => {
                let json: serde_json::Value = resp.json().await.unwrap_or_default();
                let latest = json.pointer("/dist-tags/latest")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                
                let is_deprec = json.pointer("/deprecated").is_some();
                let status = if is_deprec {
                    PackageStatus::Deprecated
                } else {
                    PackageStatus::Valid
                };

                PackageVerificationReport {
                    ecosystem: PackageEcosystem::Npm,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: latest,
                    status,
                    is_hallucinated: false,
                    is_deprecated: is_deprec,
                    is_yanked: false,
                    warning_message: None,
                    known_alternatives: vec![],
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
            _ => PackageVerificationReport {
                ecosystem: PackageEcosystem::Npm,
                name: name.to_string(),
                requested_version: requested_version.map(String::from),
                latest_version: None,
                status: PackageStatus::RegistryUnavailable,
                is_hallucinated: false,
                is_deprecated: false,
                is_yanked: false,
                warning_message: Some("Registry lookup timed out or network unavailable".to_string()),
                known_alternatives: vec![],
                cached: false,
                verification_time_ms: start.elapsed().as_millis() as u64,
            },
        }
    }

    async fn query_pypi(
        &self,
        name: &str,
        requested_version: Option<&str>,
        start: Instant,
    ) -> PackageVerificationReport {
        let url = format!("https://pypi.org/pypi/{}/json", name);
        let res = self.client.get(&url).send().await;
        match res {
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                let alt = Self::get_suggested_alternatives(name, PackageEcosystem::PyPi);
                PackageVerificationReport {
                    ecosystem: PackageEcosystem::PyPi,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: None,
                    status: PackageStatus::NotFound,
                    is_hallucinated: true,
                    is_deprecated: false,
                    is_yanked: false,
                    warning_message: Some(format!("PyPI package '{}' not found", name)),
                    known_alternatives: alt,
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
            Ok(resp) if resp.status().is_success() => {
                let json: serde_json::Value = resp.json().await.unwrap_or_default();
                let latest = json.pointer("/info/version")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let is_yanked = json.pointer("/info/yanked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let status = if is_yanked {
                    PackageStatus::Yanked
                } else {
                    PackageStatus::Valid
                };

                PackageVerificationReport {
                    ecosystem: PackageEcosystem::PyPi,
                    name: name.to_string(),
                    requested_version: requested_version.map(String::from),
                    latest_version: latest,
                    status,
                    is_hallucinated: false,
                    is_deprecated: false,
                    is_yanked,
                    warning_message: None,
                    known_alternatives: vec![],
                    cached: false,
                    verification_time_ms: start.elapsed().as_millis() as u64,
                }
            }
            _ => PackageVerificationReport {
                ecosystem: PackageEcosystem::PyPi,
                name: name.to_string(),
                requested_version: requested_version.map(String::from),
                latest_version: None,
                status: PackageStatus::RegistryUnavailable,
                is_hallucinated: false,
                is_deprecated: false,
                is_yanked: false,
                warning_message: Some("Registry lookup timed out or network unavailable".to_string()),
                known_alternatives: vec![],
                cached: false,
                verification_time_ms: start.elapsed().as_millis() as u64,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_package_guard_heuristic_hallucination() {
        let guard = PackageGuard::default();
        let rep = guard.verify_package(PackageEcosystem::CratesIo, "tokio-curl-official-rust", None).await;
        assert!(rep.is_hallucinated);
        assert_eq!(rep.status, PackageStatus::SuspiciousHallucination);
        assert!(!rep.known_alternatives.is_empty());
    }

    #[tokio::test]
    async fn test_package_guard_caching() {
        let guard = PackageGuard::default();
        let rep1 = guard.verify_package(PackageEcosystem::CratesIo, "tokio-curl", None).await;
        assert!(!rep1.cached);
        let rep2 = guard.verify_package(PackageEcosystem::CratesIo, "tokio-curl", None).await;
        assert!(rep2.cached);
    }
}
