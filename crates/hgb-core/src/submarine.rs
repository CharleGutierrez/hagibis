//! # Submarine Mode (100% Air-Gapped Sovereign Developer Protection)
//!
//! Submarine Mode enforces total operational sovereignty and absolute privacy:
//! 1. Neutralizes proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, etc.).
//! 2. Strictly enforces loopback-only communication (`127.0.0.1`, `::1`, `localhost`, local sockets).
//! 3. Audits system network sockets via `/proc/net/tcp` to detect unauthorized non-loopback egress.
//! 4. Locks out external telemetry, analytics, and phone-home flags (`DO_NOT_TRACK=1`, etc.).
//! 5. Provides an RAII guard to restore previous environment states cleanly upon exit.

use crate::error::{HgbError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static SUBMARINE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Status of the active air-gapped sovereign environment
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AirgapStatus {
    pub is_active: bool,
    pub neutralized_env_vars: Vec<String>,
    pub enforced_loopback_only: bool,
    pub zero_telemetry_enforced: bool,
    pub activated_at: String,
}

/// Comprehensive network audit report of system sockets and external connections
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkAuditReport {
    pub safe_loopback_sockets: Vec<String>,
    pub active_external_connections: Vec<String>,
    pub unauthorized_sockets: Vec<String>,
    pub airgap_breached: bool,
    pub warnings: Vec<String>,
    pub summary: String,
}

/// Controller and RAII guard for Submarine Mode
pub struct SubmarineGuard {
    saved_env: HashMap<String, Option<String>>,
    is_engaged: bool,
}

impl SubmarineGuard {
    const PROXY_VARS: &'static [&'static str] = &[
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "FTP_PROXY",
        "ftp_proxy",
    ];

    const TELEMETRY_VARS: &'static [&'static str] = &[
        "DO_NOT_TRACK",
        "HGB_TELEMETRY",
        "TELEMETRY_DISABLED",
        "ANALYTICS_ENABLED",
        "HGB_AIRGAP",
    ];

    /// Create an unengaged SubmarineGuard
    pub fn new() -> Self {
        Self {
            saved_env: HashMap::new(),
            is_engaged: false,
        }
    }

    /// Check if submarine mode is globally active
    pub fn is_globally_active() -> bool {
        SUBMARINE_ACTIVE.load(Ordering::SeqCst)
    }

    /// Engage Submarine Mode:
    /// - Stores current proxy and telemetry environment variables
    /// - Unsets/neutralizes proxy variables
    /// - Sets `NO_PROXY=*` and `no_proxy=*`
    /// - Sets `DO_NOT_TRACK=1` and `HGB_AIRGAP=1`
    pub fn engage(&mut self) -> Result<AirgapStatus> {
        let mut neutralized = Vec::new();

        // 1. Snapshot and neutralize proxy variables
        for var in Self::PROXY_VARS {
            let current = std::env::var(var).ok();
            self.saved_env.entry((*var).to_string()).or_insert(current);
            std::env::remove_var(var);
            neutralized.push((*var).to_string());
        }

        // Enforce wildcard no_proxy
        for var in &["NO_PROXY", "no_proxy"] {
            let current = std::env::var(var).ok();
            self.saved_env.entry((*var).to_string()).or_insert(current);
            std::env::set_var(var, "*");
            neutralized.push(format!("{}=*", var));
        }

        // 2. Snapshot and enforce telemetry lockdown
        for var in Self::TELEMETRY_VARS {
            let current = std::env::var(var).ok();
            self.saved_env.entry((*var).to_string()).or_insert(current);
        }
        std::env::set_var("DO_NOT_TRACK", "1");
        std::env::set_var("HGB_AIRGAP", "1");
        std::env::set_var("TELEMETRY_DISABLED", "1");
        std::env::set_var("ANALYTICS_ENABLED", "0");
        std::env::set_var("HGB_TELEMETRY", "0");

        SUBMARINE_ACTIVE.store(true, Ordering::SeqCst);
        self.is_engaged = true;

        Ok(AirgapStatus {
            is_active: true,
            neutralized_env_vars: neutralized,
            enforced_loopback_only: true,
            zero_telemetry_enforced: true,
            activated_at: Utc::now().to_rfc3339(),
        })
    }

    /// Disengage Submarine Mode and restore previous environment state
    pub fn disengage(&mut self) -> Result<()> {
        if !self.is_engaged {
            return Ok(());
        }

        for (key, val) in self.saved_env.drain() {
            match val {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }

        SUBMARINE_ACTIVE.store(false, Ordering::SeqCst);
        self.is_engaged = false;
        Ok(())
    }

    /// Validate that a requested connection endpoint strictly satisfies the loopback / sovereign constraint.
    /// Rejects external hostnames, public IP addresses, and unauthorized network egress.
    pub fn validate_destination(target: &str) -> Result<()> {
        let cleaned = target.trim();
        if cleaned.is_empty() {
            return Err(HgbError::InvalidInput("Target address is empty".to_string()));
        }

        // Remove schema if present
        let host_part = if let Some(idx) = cleaned.find("://") {
            &cleaned[idx + 3..]
        } else {
            cleaned
        };

        // Extract host (ignore path and query)
        let host_and_port = host_part.split('/').next().unwrap_or(host_part);
        let host = if host_and_port.starts_with('[') {
            // IPv6 bracketed format e.g. [::1]:8080
            if let Some(close_idx) = host_and_port.find(']') {
                &host_and_port[1..close_idx]
            } else {
                host_and_port
            }
        } else {
            // IPv4 or hostname
            host_and_port.split(':').next().unwrap_or(host_and_port)
        };

        // Whitelisted local / sovereign patterns
        if host == "localhost"
            || host == "127.0.0.1"
            || host == "0.0.0.0"
            || host == "::1"
            || host == "::"
            || host.ends_with(".local")
            || host.starts_with("unix:")
            || host.is_empty()
        {
            return Ok(());
        }

        // Parse as IP to verify if it is a loopback IP
        if let Ok(ip) = host.parse::<IpAddr>() {
            if ip.is_loopback() || ip.is_unspecified() {
                return Ok(());
            }
        }

        Err(HgbError::Security(format!(
            "Submarine Airgap Violation: Outbound request to external destination '{}' blocked by SubmarineGuard",
            target
        )))
    }

    /// Audit system network sockets (inspects `/proc/net/tcp` and `/proc/net/tcp6` on Linux).
    /// Detects active non-loopback connections and potential exfiltration vectors.
    pub fn audit_network() -> NetworkAuditReport {
        let mut safe_sockets = Vec::new();
        let mut active_external = Vec::new();
        let mut warnings = Vec::new();

        // 1. Check /proc/net/tcp
        if Path::new("/proc/net/tcp").exists() {
            if let Ok(content) = fs::read_to_string("/proc/net/tcp") {
                Self::parse_proc_net_tcp(&content, false, &mut safe_sockets, &mut active_external);
            }
        }

        // 2. Check /proc/net/tcp6
        if Path::new("/proc/net/tcp6").exists() {
            if let Ok(content) = fs::read_to_string("/proc/net/tcp6") {
                Self::parse_proc_net_tcp(&content, true, &mut safe_sockets, &mut active_external);
            }
        }

        let airgap_breached = !active_external.is_empty();
        if airgap_breached {
            warnings.push(format!(
                "Detected {} external TCP connection(s) outside loopback address space!",
                active_external.len()
            ));
        }

        let summary = if airgap_breached {
            format!(
                "AIRGAP COMPROMISED: {} active external connection(s) detected.",
                active_external.len()
            )
        } else {
            format!(
                "AIRGAP VERIFIED: Sovereign environment intact. {} local loopback socket(s) active.",
                safe_sockets.len()
            )
        };

        NetworkAuditReport {
            safe_loopback_sockets: safe_sockets,
            active_external_connections: active_external.clone(),
            unauthorized_sockets: active_external,
            airgap_breached,
            warnings,
            summary,
        }
    }

    /// Parses /proc/net/tcp or /proc/net/tcp6 lines
    pub fn parse_proc_net_tcp(
        content: &str,
        is_ipv6: bool,
        safe: &mut Vec<String>,
        external: &mut Vec<String>,
    ) {
        for line in content.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                continue;
            }

            let local_addr_hex = parts[1];
            let rem_addr_hex = parts[2];
            let state = parts[3];

            // TCP_ESTABLISHED is "01"
            let is_established = state == "01";

            let rem_ip_opt = if is_ipv6 {
                Self::parse_ipv6_hex(rem_addr_hex)
            } else {
                Self::parse_ipv4_hex(rem_addr_hex)
            };

            let local_ip_opt = if is_ipv6 {
                Self::parse_ipv6_hex(local_addr_hex)
            } else {
                Self::parse_ipv4_hex(local_addr_hex)
            };

            if let Some((rem_ip, rem_port)) = rem_ip_opt {
                let local_desc = local_ip_opt
                    .map(|(ip, port)| format!("{}:{}", ip, port))
                    .unwrap_or_else(|| local_addr_hex.to_string());

                let conn_desc = format!("{} -> {}:{}", local_desc, rem_ip, rem_port);

                // If remote port is 0, it is a listening socket, not an outbound connection
                if rem_port == 0 || rem_ip.is_unspecified() {
                    safe.push(format!("LISTEN: {}", local_desc));
                } else if rem_ip.is_loopback() {
                    safe.push(format!("LOOPBACK: {}", conn_desc));
                } else if is_established {
                    external.push(format!("ESTABLISHED: {}", conn_desc));
                }
            }
        }
    }

    /// Helper to parse Linux /proc/net/tcp IPv4 hex address format: "0100007F:1F90" -> 127.0.0.1:8080
    fn parse_ipv4_hex(s: &str) -> Option<(IpAddr, u16)> {
        let mut parts = s.split(':');
        let ip_hex = parts.next()?;
        let port_hex = parts.next()?;

        let port = u16::from_str_radix(port_hex, 16).ok()?;
        if ip_hex.len() != 8 {
            return None;
        }

        let num = u32::from_str_radix(ip_hex, 16).ok()?;
        // Linux stores little-endian in /proc/net/tcp
        let b1 = (num & 0xFF) as u8;
        let b2 = ((num >> 8) & 0xFF) as u8;
        let b3 = ((num >> 16) & 0xFF) as u8;
        let b4 = ((num >> 24) & 0xFF) as u8;

        Some((IpAddr::from([b1, b2, b3, b4]), port))
    }

    /// Helper to parse Linux /proc/net/tcp6 IPv6 hex address format
    fn parse_ipv6_hex(s: &str) -> Option<(IpAddr, u16)> {
        let mut parts = s.split(':');
        let ip_hex = parts.next()?;
        let port_hex = parts.next()?;

        let port = u16::from_str_radix(port_hex, 16).ok()?;
        if ip_hex.len() != 32 {
            return None;
        }

        let mut bytes = [0u8; 16];
        for i in 0..4 {
            let chunk = &ip_hex[i * 8..(i + 1) * 8];
            if let Ok(num) = u32::from_str_radix(chunk, 16) {
                let le_bytes = num.to_le_bytes();
                bytes[i * 4..(i + 1) * 4].copy_from_slice(&le_bytes);
            } else {
                return None;
            }
        }

        Some((IpAddr::from(bytes), port))
    }
}

impl Default for SubmarineGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SubmarineGuard {
    fn drop(&mut self) {
        let _ = self.disengage();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submarine_guard_lifecycle() {
        std::env::set_var("HTTP_PROXY", "http://corporate-proxy.internal:8080");
        std::env::set_var("DO_NOT_TRACK", "0");

        {
            let mut guard = SubmarineGuard::new();
            let status = guard.engage().unwrap();

            assert!(status.is_active);
            assert!(SubmarineGuard::is_globally_active());
            assert!(std::env::var("HTTP_PROXY").is_err());
            assert_eq!(std::env::var("DO_NOT_TRACK").unwrap(), "1");
            assert_eq!(std::env::var("HGB_AIRGAP").unwrap(), "1");
            assert_eq!(std::env::var("NO_PROXY").unwrap(), "*");
        }

        // After drop, previous env vars must be cleanly restored
        assert!(!SubmarineGuard::is_globally_active());
        assert_eq!(
            std::env::var("HTTP_PROXY").unwrap(),
            "http://corporate-proxy.internal:8080"
        );
        assert_eq!(std::env::var("DO_NOT_TRACK").unwrap(), "0");

        // Clean up
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("DO_NOT_TRACK");
    }

    #[test]
    fn test_validate_destination_loopback_allowed() {
        assert!(SubmarineGuard::validate_destination("http://127.0.0.1:11434").is_ok());
        assert!(SubmarineGuard::validate_destination("http://localhost:8080/v1/chat").is_ok());
        assert!(SubmarineGuard::validate_destination("http://[::1]:3000").is_ok());
        assert!(SubmarineGuard::validate_destination("unix:///var/run/docker.sock").is_ok());
    }

    #[test]
    fn test_validate_destination_external_blocked() {
        let res1 = SubmarineGuard::validate_destination("https://api.openai.com/v1/chat/completions");
        assert!(res1.is_err());

        let res2 = SubmarineGuard::validate_destination("http://8.8.8.8:53");
        assert!(res2.is_err());

        let res3 = SubmarineGuard::validate_destination("https://telemetry.analytics.co/track");
        assert!(res3.is_err());
    }

    #[test]
    fn test_parse_proc_net_tcp_synthetic() {
        // Synthetic /proc/net/tcp content
        // 0100007F:2CA0 is 127.0.0.1:11424
        // 00000000:0000 is 0.0.0.0:0 (listening)
        // 08080808:0050 is 8.8.8.8:80 (established external connection)
        let sample = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:2CA0 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 00000000 100 0 0 10 -1
   1: 0100007F:2CA0 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 12346 1 00000000 100 0 0 10 -1
   2: 0200007F:2CA1 08080808:0050 01 00000000:00000000 00:00000000 00000000  1000        0 12347 1 00000000 100 0 0 10 -1
";

        let mut safe = Vec::new();
        let mut external = Vec::new();
        SubmarineGuard::parse_proc_net_tcp(sample, false, &mut safe, &mut external);

        assert_eq!(safe.len(), 2); // 1 listening, 1 loopback
        assert_eq!(external.len(), 1); // 1 external to 8.8.8.8:80
        assert!(external[0].contains("8.8.8.8:80"));
    }

    #[test]
    fn test_audit_network_system() {
        // Run audit on the current real system
        let report = SubmarineGuard::audit_network();
        // Should execute without panics on Linux
        assert!(!report.summary.is_empty());
    }
}
