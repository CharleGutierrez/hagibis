use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::ops::RangeInclusive;

/// Discovered port status and collision mitigation details
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortCollision {
    pub service_name: String,
    pub original_port: u16,
    pub is_colliding: bool,
    pub allocated_port: u16,
    pub env_var: String,
}

/// Unified gateway reverse-proxy route entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRoute {
    pub path_prefix: String,
    pub target_service: String,
    pub upstream_url: String,
}

/// Comprehensive port multiplexing resolution report
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortMultiplexerReport {
    pub scanned_ports: usize,
    pub collisions: Vec<PortCollision>,
    pub env_overrides_content: String,
    pub gateway_routes: Vec<GatewayRoute>,
}

/// Auto-Port Multiplexer & DevServer Traffic Resolver
pub struct PortMultiplexer;

impl PortMultiplexer {
    /// Probe whether a TCP port on localhost is currently bound
    pub fn is_port_in_use(port: u16) -> bool {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                drop(listener);
                false
            }
            Err(_) => true,
        }
    }

    /// Search for an available port within the specified inclusive range
    pub fn find_free_port_in_range(range: RangeInclusive<u16>) -> Option<u16> {
        for port in range {
            if !Self::is_port_in_use(port) {
                return Some(port);
            }
        }
        None
    }

    /// Allocate an available OS ephemeral port
    pub fn find_ephemeral_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .unwrap_or(49152)
    }

    /// Resolve all standard developer server ranges and generate collision mitigations
    pub fn resolve_service_ports() -> PortMultiplexerReport {
        let services = [
            ("Frontend (React/Next)", 3000..=3010, "PORT"),
            ("Vite DevServer", 5173..=5183, "VITE_PORT"),
            ("FastAPI / Python", 8000..=8010, "FASTAPI_PORT"),
            ("Axum / Actix Microservice", 8080..=8090, "SERVER_PORT"),
        ];

        let mut collisions = Vec::new();
        let mut routes = Vec::new();
        let mut env_lines = Vec::new();
        let mut total_scanned = 0;

        for (svc_name, range, env_var) in services {
            total_scanned += range.end() - range.start() + 1;
            let base_port = *range.start();
            let is_colliding = Self::is_port_in_use(base_port);

            let allocated = if is_colliding {
                Self::find_free_port_in_range(range).unwrap_or_else(Self::find_ephemeral_port)
            } else {
                base_port
            };

            collisions.push(PortCollision {
                service_name: svc_name.to_string(),
                original_port: base_port,
                is_colliding,
                allocated_port: allocated,
                env_var: env_var.to_string(),
            });

            env_lines.push(format!("{}={}", env_var, allocated));

            let route_prefix = match base_port {
                8000 | 8080 => "/api",
                _ => "/",
            };
            routes.push(GatewayRoute {
                path_prefix: route_prefix.to_string(),
                target_service: svc_name.to_string(),
                upstream_url: format!("http://127.0.0.1:{}", allocated),
            });
        }

        PortMultiplexerReport {
            scanned_ports: total_scanned as usize,
            collisions,
            env_overrides_content: env_lines.join("\n"),
            gateway_routes: routes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ephemeral_port_allocation() {
        let port = PortMultiplexer::find_ephemeral_port();
        assert!(port > 1024);
    }

    #[test]
    fn test_resolve_service_ports() {
        let report = PortMultiplexer::resolve_service_ports();
        assert_eq!(report.collisions.len(), 4);
        assert!(report.env_overrides_content.contains("PORT="));
        assert!(report.env_overrides_content.contains("VITE_PORT="));
        assert!(report.env_overrides_content.contains("FASTAPI_PORT="));
        assert!(!report.gateway_routes.is_empty());
    }
}
