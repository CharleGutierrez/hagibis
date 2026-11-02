//! # Pillar III: Edge Prompt Cache Router (edge_prompt_cache_router)
//!
//! Cloudflare Worker & Wasm synergy router pushing prompt pre-processing,
//! hierarchical BLAKE3 hashing, and KV cache hit verification straight to the edge.

use blake3::Hasher;
use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

// 1. Cloudflare Configuration (wrangler.toml & _routes.json)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WranglerConfig {
    pub name: String,
    pub compatibility_date: String,
    pub pages_build_output_dir: String,
    pub vars: HashMap<String, String>,
}

impl Default for WranglerConfig {
    fn default() -> Self {
        let mut vars = HashMap::new();
        vars.insert("ENVIRONMENT".to_string(), "production".to_string());
        vars.insert("DEPLOYED_BY".to_string(), "hagibis-edge-deployer".to_string());
        Self {
            name: "my-vibe-app".to_string(),
            compatibility_date: "2026-09-01".to_string(),
            pages_build_output_dir: ".".to_string(),
            vars,
        }
    }
}

impl WranglerConfig {
    pub fn parse(content: &str) -> Self {
        let mut config = Self::default();
        let mut in_vars = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            if trimmed == "[vars]" {
                in_vars = true;
                continue;
            } else if trimmed.starts_with('[') {
                in_vars = false;
                continue;
            }

            if let Some((k, v)) = trimmed.split_once('=') {
                let key = k.trim().to_string();
                let val = v.trim().trim_matches('"').to_string();
                if in_vars {
                    config.vars.insert(key, val);
                } else {
                    match key.as_str() {
                        "name" => config.name = val,
                        "compatibility_date" => config.compatibility_date = val,
                        "pages_build_output_dir" => config.pages_build_output_dir = val,
                        _ => {}
                    }
                }
            }
        }
        config
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            HgbError::Storage(format!("Failed to read wrangler.toml at {}: {}", path.display(), e))
        })?;
        Ok(Self::parse(&content))
    }

    pub fn environment(&self) -> &str {
        self.vars.get("ENVIRONMENT").map(|s| s.as_str()).unwrap_or("production")
    }

    pub fn deployed_by(&self) -> &str {
        self.vars.get("DEPLOYED_BY").map(|s| s.as_str()).unwrap_or("hagibis-edge-deployer")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudflareRoutes {
    pub version: u32,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for CloudflareRoutes {
    fn default() -> Self {
        Self {
            version: 1,
            include: vec!["/*".to_string()],
            exclude: vec![
                "/static/*".to_string(),
                "/assets/*".to_string(),
                "/favicon.ico".to_string(),
            ],
        }
    }
}

impl CloudflareRoutes {
    pub fn parse(content: &str) -> Result<Self> {
        serde_json::from_str(content).map_err(|e| {
            HgbError::Serialization(format!("Failed to parse _routes.json: {}", e))
        })
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            HgbError::Storage(format!("Failed to read _routes.json at {}: {}", path.display(), e))
        })?;
        Self::parse(&content)
    }

    pub fn is_edge_routed(&self, path: &str) -> bool {
        for pat in &self.exclude {
            if Self::glob_matches(pat, path) {
                return false;
            }
        }
        if self.include.is_empty() {
            return true;
        }
        for pat in &self.include {
            if Self::glob_matches(pat, path) {
                return true;
            }
        }
        false
    }

    pub fn glob_matches(pattern: &str, path: &str) -> bool {
        if pattern == "/*" || pattern == path {
            return true;
        }
        if let Some(prefix) = pattern.strip_suffix("/*") {
            return path.starts_with(prefix);
        }
        if let Some(suffix) = pattern.strip_prefix('*') {
            return path.ends_with(suffix);
        }
        false
    }
}

// 2. Hierarchical Prompt Hashing
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptComponents {
    pub prefix: String,
    pub system_prompt: String,
    pub context: String,
    pub user_query: String,
}

impl PromptComponents {
    pub fn new(
        prefix: impl Into<String>,
        system_prompt: impl Into<String>,
        context: impl Into<String>,
        user_query: impl Into<String>,
    ) -> Self {
        Self {
            prefix: prefix.into(),
            system_prompt: system_prompt.into(),
            context: context.into(),
            user_query: user_query.into(),
        }
    }

    pub fn simple(system: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            prefix: "<|prompt_start|>".to_string(),
            system_prompt: system.into(),
            context: String::new(),
            user_query: query.into(),
        }
    }

    pub fn total_char_len(&self) -> usize {
        self.prefix.len() + self.system_prompt.len() + self.context.len() + self.user_query.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchicalPromptHash {
    pub prefix_hash: String,
    pub system_hash: String,
    pub context_hash: String,
    pub query_hash: String,
    pub full_prompt_hash: String,
    pub estimated_tokens: usize,
}

impl HierarchicalPromptHash {
    pub fn compute(components: &PromptComponents) -> Self {
        let mut h1 = Hasher::new();
        h1.update(b"PREFIX:");
        h1.update(components.prefix.as_bytes());
        let prefix_hash = h1.finalize().to_hex().to_string();

        let mut h2 = Hasher::new();
        h2.update(b"SYS:");
        h2.update(prefix_hash.as_bytes());
        h2.update(b"\n");
        h2.update(components.system_prompt.as_bytes());
        let system_hash = h2.finalize().to_hex().to_string();

        let mut h3 = Hasher::new();
        h3.update(b"CTX:");
        h3.update(system_hash.as_bytes());
        h3.update(b"\n");
        h3.update(components.context.as_bytes());
        let context_hash = h3.finalize().to_hex().to_string();

        let mut h4 = Hasher::new();
        h4.update(b"QRY:");
        h4.update(context_hash.as_bytes());
        h4.update(b"\n");
        h4.update(components.user_query.as_bytes());
        let query_hash = h4.finalize().to_hex().to_string();

        let mut h_full = Hasher::new();
        h_full.update(prefix_hash.as_bytes());
        h_full.update(b":");
        h_full.update(system_hash.as_bytes());
        h_full.update(b":");
        h_full.update(context_hash.as_bytes());
        h_full.update(b":");
        h_full.update(query_hash.as_bytes());
        let full_prompt_hash = h_full.finalize().to_hex().to_string();

        let estimated_tokens = (components.total_char_len() / 4).max(1);

        Self {
            prefix_hash,
            system_hash,
            context_hash,
            query_hash,
            full_prompt_hash,
            estimated_tokens,
        }
    }

    pub fn matches_exact(&self, other: &Self) -> bool {
        self.full_prompt_hash == other.full_prompt_hash
    }

    pub fn matches_context(&self, other: &Self) -> bool {
        self.context_hash == other.context_hash
    }

    pub fn matches_system(&self, other: &Self) -> bool {
        self.system_hash == other.system_hash
    }
}

// 3. Edge Cache Storage
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedPromptResponse {
    pub prompt_hash: HierarchicalPromptHash,
    pub response_text: String,
    pub cached_tokens: Vec<u32>,
    pub kv_fingerprint: String,
    pub embedding: Option<Vec<f32>>,
    pub created_at_ms: u64,
    pub ttl_secs: u64,
    pub hit_count: u64,
    pub last_accessed_ms: u64,
}

impl CachedPromptResponse {
    pub fn is_expired(&self) -> bool {
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        now_ms > self.created_at_ms + (self.ttl_secs * 1000)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedPrefixKv {
    pub context_hash: String,
    pub system_hash: String,
    pub prefix_tokens: Vec<u32>,
    pub kv_fingerprint: String,
    pub created_at_ms: u64,
    pub ttl_secs: u64,
}

impl CachedPrefixKv {
    pub fn is_expired(&self) -> bool {
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        now_ms > self.created_at_ms + (self.ttl_secs * 1000)
    }
}

#[derive(Debug, Clone)]
pub enum CacheLookupResult {
    ExactHit(CachedPromptResponse),
    PrefixHit {
        prefix_kv: CachedPrefixKv,
        reusable_tokens_estimate: usize,
    },
    Miss,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EdgeCacheStats {
    pub exact_hits: u64,
    pub prefix_hits: u64,
    pub misses: u64,
    pub total_lookups: u64,
    pub hit_ratio: f64,
    pub saved_tokens_total: u64,
    pub capacity: usize,
    pub cached_entries_count: usize,
}

pub struct EdgePromptCache {
    exact_cache: RwLock<HashMap<String, CachedPromptResponse>>,
    prefix_kv_cache: RwLock<HashMap<String, CachedPrefixKv>>,
    stats: RwLock<EdgeCacheStats>,
    capacity: usize,
}

impl EdgePromptCache {
    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 { 2048 } else { capacity };
        Self {
            exact_cache: RwLock::new(HashMap::with_capacity(cap)),
            prefix_kv_cache: RwLock::new(HashMap::with_capacity(cap)),
            stats: RwLock::new(EdgeCacheStats {
                capacity: cap,
                ..Default::default()
            }),
            capacity: cap,
        }
    }

    pub fn lookup(&self, hash: &HierarchicalPromptHash) -> CacheLookupResult {
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        if let Ok(mut cache) = self.exact_cache.write() {
            if let Some(entry) = cache.get_mut(&hash.full_prompt_hash) {
                if !entry.is_expired() {
                    entry.hit_count += 1;
                    entry.last_accessed_ms = now_ms;
                    let result = entry.clone();

                    if let Ok(mut st) = self.stats.write() {
                        st.exact_hits += 1;
                        st.total_lookups += 1;
                        st.saved_tokens_total += hash.estimated_tokens as u64;
                        st.hit_ratio = (st.exact_hits + st.prefix_hits) as f64 / st.total_lookups as f64;
                    }
                    return CacheLookupResult::ExactHit(result);
                }
            }
        }

        if let Ok(prefix_cache) = self.prefix_kv_cache.read() {
            if let Some(prefix_entry) = prefix_cache.get(&hash.context_hash) {
                if !prefix_entry.is_expired() {
                    let reusable_tokens = prefix_entry.prefix_tokens.len();
                    let prefix_kv = prefix_entry.clone();

                    if let Ok(mut st) = self.stats.write() {
                        st.prefix_hits += 1;
                        st.total_lookups += 1;
                        st.saved_tokens_total += reusable_tokens as u64;
                        st.hit_ratio = (st.exact_hits + st.prefix_hits) as f64 / st.total_lookups as f64;
                    }
                    return CacheLookupResult::PrefixHit {
                        prefix_kv,
                        reusable_tokens_estimate: reusable_tokens,
                    };
                }
            }
        }

        if let Ok(mut st) = self.stats.write() {
            st.misses += 1;
            st.total_lookups += 1;
            st.hit_ratio = (st.exact_hits + st.prefix_hits) as f64 / st.total_lookups as f64;
        }

        CacheLookupResult::Miss
    }

    pub fn insert_exact(&self, entry: CachedPromptResponse) {
        if let Ok(mut cache) = self.exact_cache.write() {
            if cache.len() >= self.capacity {
                if let Some(key) = cache.keys().next().cloned() {
                    cache.remove(&key);
                }
            }
            cache.insert(entry.prompt_hash.full_prompt_hash.clone(), entry);
            if let Ok(mut st) = self.stats.write() {
                st.cached_entries_count = cache.len();
            }
        }
    }

    pub fn insert_prefix_kv(&self, prefix: CachedPrefixKv) {
        if let Ok(mut prefix_cache) = self.prefix_kv_cache.write() {
            if prefix_cache.len() >= self.capacity {
                if let Some(key) = prefix_cache.keys().next().cloned() {
                    prefix_cache.remove(&key);
                }
            }
            prefix_cache.insert(prefix.context_hash.clone(), prefix);
        }
    }

    pub fn purge_expired(&self) -> usize {
        let mut purged = 0;
        if let Ok(mut cache) = self.exact_cache.write() {
            cache.retain(|_, v| {
                if v.is_expired() { purged += 1; false } else { true }
            });
        }
        if let Ok(mut prefix_cache) = self.prefix_kv_cache.write() {
            prefix_cache.retain(|_, v| {
                if v.is_expired() { purged += 1; false } else { true }
            });
        }
        purged
    }

    pub fn stats(&self) -> EdgeCacheStats {
        self.stats.read().map(|s| s.clone()).unwrap_or_default()
    }
}

// 4. Routing Decisions
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeRouteAction {
    EdgeCacheHit,
    PrefixCacheHitForward,
    CacheMissForward,
    BypassedDirect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RoutingDestination {
    EdgeWorkerCache,
    LocalDaemon { endpoint: String },
    FirecrackerMicroVm { socket_path: String, vm_id: String },
    StaticAssetBypass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRouteDecision {
    pub action: EdgeRouteAction,
    pub destination: RoutingDestination,
    pub prompt_hash: HierarchicalPromptHash,
    pub cached_response: Option<String>,
    pub cached_tokens: Vec<u32>,
    pub reusable_token_count: usize,
    pub remaining_query_tokens: usize,
    pub estimated_latency_savings_ms: u64,
    pub compute_savings_pct: f32,
}

// 5. Edge Prompt Cache Router
pub struct EdgePromptCacheRouter {
    pub wrangler_config: WranglerConfig,
    pub routes_config: CloudflareRoutes,
    pub cache: Arc<EdgePromptCache>,
    pub local_daemon_endpoint: String,
    pub microvm_socket_path: String,
    pub prefer_microvm: bool,
}

impl EdgePromptCacheRouter {
    pub fn new(daemon_endpoint: impl Into<String>, microvm_socket: impl Into<String>) -> Self {
        Self {
            wrangler_config: WranglerConfig::default(),
            routes_config: CloudflareRoutes::default(),
            cache: Arc::new(EdgePromptCache::new(2048)),
            local_daemon_endpoint: daemon_endpoint.into(),
            microvm_socket_path: microvm_socket.into(),
            prefer_microvm: false,
        }
    }

    pub fn with_configs(
        wrangler_path: &Path,
        routes_path: &Path,
        daemon_endpoint: &str,
        microvm_socket: &str,
    ) -> Result<Self> {
        let wrangler = WranglerConfig::load_from_file(wrangler_path)?;
        let routes = CloudflareRoutes::load_from_file(routes_path)?;
        Ok(Self {
            wrangler_config: wrangler,
            routes_config: routes,
            cache: Arc::new(EdgePromptCache::new(2048)),
            local_daemon_endpoint: daemon_endpoint.to_string(),
            microvm_socket_path: microvm_socket.to_string(),
            prefer_microvm: false,
        })
    }

    pub fn set_prefer_microvm(&mut self, prefer: bool) {
        self.prefer_microvm = prefer;
    }

    pub fn route_prompt(&self, path: &str, prompt: &PromptComponents) -> EdgeRouteDecision {
        let hash = HierarchicalPromptHash::compute(prompt);

        if !self.routes_config.is_edge_routed(path) {
            return EdgeRouteDecision {
                action: EdgeRouteAction::BypassedDirect,
                destination: RoutingDestination::StaticAssetBypass,
                prompt_hash: hash,
                cached_response: None,
                cached_tokens: Vec::new(),
                reusable_token_count: 0,
                remaining_query_tokens: 0,
                estimated_latency_savings_ms: 0,
                compute_savings_pct: 0.0,
            };
        }

        match self.cache.lookup(&hash) {
            CacheLookupResult::ExactHit(cached) => EdgeRouteDecision {
                action: EdgeRouteAction::EdgeCacheHit,
                destination: RoutingDestination::EdgeWorkerCache,
                prompt_hash: hash.clone(),
                cached_response: Some(cached.response_text),
                cached_tokens: cached.cached_tokens,
                reusable_token_count: hash.estimated_tokens,
                remaining_query_tokens: 0,
                estimated_latency_savings_ms: 1200,
                compute_savings_pct: 100.0,
            },
            CacheLookupResult::PrefixHit {
                prefix_kv,
                reusable_tokens_estimate,
            } => {
                let remaining = hash.estimated_tokens.saturating_sub(reusable_tokens_estimate);
                let savings = if hash.estimated_tokens > 0 {
                    (reusable_tokens_estimate as f32 / hash.estimated_tokens as f32) * 100.0
                } else {
                    50.0
                };
                let destination = if self.prefer_microvm {
                    RoutingDestination::FirecrackerMicroVm {
                        socket_path: self.microvm_socket_path.clone(),
                        vm_id: "vm-edge-worker".to_string(),
                    }
                } else {
                    RoutingDestination::LocalDaemon {
                        endpoint: self.local_daemon_endpoint.clone(),
                    }
                };

                EdgeRouteDecision {
                    action: EdgeRouteAction::PrefixCacheHitForward,
                    destination,
                    prompt_hash: hash,
                    cached_response: None,
                    cached_tokens: prefix_kv.prefix_tokens,
                    reusable_token_count: reusable_tokens_estimate,
                    remaining_query_tokens: remaining,
                    estimated_latency_savings_ms: 600,
                    compute_savings_pct: savings.min(95.0),
                }
            }
            CacheLookupResult::Miss => {
                let destination = if self.prefer_microvm {
                    RoutingDestination::FirecrackerMicroVm {
                        socket_path: self.microvm_socket_path.clone(),
                        vm_id: "vm-edge-worker".to_string(),
                    }
                } else {
                    RoutingDestination::LocalDaemon {
                        endpoint: self.local_daemon_endpoint.clone(),
                    }
                };

                EdgeRouteDecision {
                    action: EdgeRouteAction::CacheMissForward,
                    destination,
                    prompt_hash: hash.clone(),
                    cached_response: None,
                    cached_tokens: Vec::new(),
                    reusable_token_count: 0,
                    remaining_query_tokens: hash.estimated_tokens,
                    estimated_latency_savings_ms: 0,
                    compute_savings_pct: 0.0,
                }
            }
        }
    }

    pub fn cache_response(
        &self,
        prompt: &PromptComponents,
        response_text: impl Into<String>,
        tokens: Vec<u32>,
        kv_fingerprint: Option<String>,
        embedding: Option<Vec<f32>>,
        ttl_secs: u64,
    ) {
        let hash = HierarchicalPromptHash::compute(prompt);
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let fp = kv_fingerprint.unwrap_or_else(|| hash.context_hash.clone());

        let entry = CachedPromptResponse {
            prompt_hash: hash,
            response_text: response_text.into(),
            cached_tokens: tokens,
            kv_fingerprint: fp,
            embedding,
            created_at_ms: now_ms,
            ttl_secs,
            hit_count: 0,
            last_accessed_ms: now_ms,
        };
        self.cache.insert_exact(entry);
    }

    pub fn cache_prefix_kv(
        &self,
        prompt: &PromptComponents,
        prefix_tokens: Vec<u32>,
        kv_fingerprint: &str,
        ttl_secs: u64,
    ) {
        let hash = HierarchicalPromptHash::compute(prompt);
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        let prefix_kv = CachedPrefixKv {
            context_hash: hash.context_hash,
            system_hash: hash.system_hash,
            prefix_tokens,
            kv_fingerprint: kv_fingerprint.to_string(),
            created_at_ms: now_ms,
            ttl_secs,
        };
        self.cache.insert_prefix_kv(prefix_kv);
    }

    pub fn stream_cached_tokens(&self, tokens: &[u32]) -> mpsc::Receiver<u32> {
        let (tx, rx) = mpsc::channel(tokens.len().max(1));
        let token_vec = tokens.to_vec();

        tokio::spawn(async move {
            for token in token_vec {
                if tx.send(token).await.is_err() {
                    break;
                }
            }
        });

        rx
    }

    pub fn stats(&self) -> EdgeCacheStats {
        self.cache.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrangler_config_parsing() {
        let toml_str = r#"
name = "my-vibe-app"
compatibility_date = "2026-09-01"
pages_build_output_dir = "."

[vars]
ENVIRONMENT = "staging"
DEPLOYED_BY = "hagibis-test"
"#;
        let config = WranglerConfig::parse(toml_str);
        assert_eq!(config.name, "my-vibe-app");
        assert_eq!(config.environment(), "staging");
        assert_eq!(config.deployed_by(), "hagibis-test");
    }

    #[test]
    fn test_routes_filtering() {
        let json_str = r#"{
            "version": 1,
            "include": ["/*"],
            "exclude": ["/static/*", "/assets/*", "/favicon.ico"]
        }"#;
        let routes = CloudflareRoutes::parse(json_str).unwrap();
        assert!(routes.is_edge_routed("/api/prompt"));
        assert!(routes.is_edge_routed("/vibe/infer"));
        assert!(!routes.is_edge_routed("/static/bundle.js"));
        assert!(!routes.is_edge_routed("/favicon.ico"));
    }

    #[test]
    fn test_hierarchical_hashing_and_exact_cache_hit() {
        let router = EdgePromptCacheRouter::new("http://localhost:8083", "/run/user/1000/fc.sock");
        let prompt = PromptComponents::new(
            "<|im_start|>",
            "You are an expert Rust systems programmer.",
            "Workspace: hagibis",
            "Optimize memory usage"
        );

        let decision = router.route_prompt("/api/infer", &prompt);
        assert_eq!(decision.action, EdgeRouteAction::CacheMissForward);

        router.cache_response(
            &prompt,
            "Here is the optimized zero-copy implementation.",
            vec![101, 102, 103],
            None,
            None,
            3600
        );

        let decision_after = router.route_prompt("/api/infer", &prompt);
        assert_eq!(decision_after.action, EdgeRouteAction::EdgeCacheHit);
        assert_eq!(decision_after.cached_response.unwrap(), "Here is the optimized zero-copy implementation.");
        assert_eq!(decision_after.compute_savings_pct, 100.0);
    }
}
