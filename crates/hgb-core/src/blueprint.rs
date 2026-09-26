use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::error::Result;

/// Kind of architectural component identified in the codebase.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ComponentKind {
    Crate,
    Module,
    ApiRoute,
    DatabaseEntity,
    ExternalService,
    ClientInterface,
}

impl ComponentKind {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Crate => "📦 CRATE",
            Self::Module => "🧩 MOD",
            Self::ApiRoute => "🌐 ROUTE",
            Self::DatabaseEntity => "💾 ENTITY",
            Self::ExternalService => "⚡ SVC",
            Self::ClientInterface => "🖥️ UI",
        }
    }

    pub fn mermaid_shape(&self, id: &str, label: &str) -> String {
        match self {
            Self::Crate => format!("{}[[\"{}\"]]", id, label),
            Self::Module => format!("{}[\"{}\"]", id, label),
            Self::ApiRoute => format!("{}ु(\"{}\")", id, label).replace('ु', ">"), // >["label"]
            Self::DatabaseEntity => format!("{}[(\"{}\")]", id, label),
            Self::ExternalService => format!("{}{{\"{}\"}}", id, label),
            Self::ClientInterface => format!("{}[\"{}\"]", id, label),
        }
    }
}

/// A node in the architecture blueprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentNode {
    pub id: String,
    pub name: String,
    pub kind: ComponentKind,
    pub path: PathBuf,
    pub description: String,
    pub metadata: HashMap<String, String>,
}

/// A dependency or dataflow edge between components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub source_id: String,
    pub target_id: String,
    pub label: String,
}

/// Complete architectural blueprint representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureBlueprint {
    pub title: String,
    pub workspace_root: PathBuf,
    pub nodes: Vec<ComponentNode>,
    pub edges: Vec<DependencyEdge>,
    pub total_files_scanned: usize,
    pub total_routes: usize,
    pub total_entities: usize,
}

impl ArchitectureBlueprint {
    /// Creates an empty blueprint.
    pub fn new(title: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            title: title.into(),
            workspace_root: root.into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            total_files_scanned: 0,
            total_routes: 0,
            total_entities: 0,
        }
    }

    /// Scans a workspace repository and builds the living architecture blueprint.
    pub fn scan_workspace(root: &Path) -> Result<Self> {
        let mut blueprint = Self::new("Living Architecture Blueprint", root);

        // 1. Detect Rust Crates in Cargo.toml or crates/
        let crates_dir = root.join("crates");
        if crates_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&crates_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && path.join("Cargo.toml").exists() {
                        let crate_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                        let id = format!("crate_{}", crate_name.replace('-', "_"));
                        let mut meta = HashMap::new();
                        meta.insert("dir".to_string(), path.display().to_string());

                        blueprint.nodes.push(ComponentNode {
                            id: id.clone(),
                            name: crate_name.clone(),
                            kind: ComponentKind::Crate,
                            path: path.clone(),
                            description: format!("Rust crate: {}", crate_name),
                            metadata: meta,
                        });
                    }
                }
            }
        }

        // Also check root crate if Cargo.toml exists
        if root.join("Cargo.toml").exists() && blueprint.nodes.is_empty() {
            let root_name = root.file_name().unwrap_or_default().to_string_lossy().to_string();
            blueprint.nodes.push(ComponentNode {
                id: format!("crate_{}", root_name.replace('-', "_")),
                name: root_name,
                kind: ComponentKind::Crate,
                path: root.to_path_buf(),
                description: "Root project crate".to_string(),
                metadata: HashMap::new(),
            });
        }

        // 2. Scan source files recursively for modules, routes, entities, external services
        let mut files_to_scan = Vec::new();
        collect_source_files(root, &mut files_to_scan, 0, 5);
        let scanned_files = files_to_scan.len();

        let mut discovered_entities = HashSet::new();
        let mut discovered_routes = HashSet::new();

        for file in &files_to_scan {
            if let Ok(content) = fs::read_to_string(file) {
                let rel_path = file.strip_prefix(root).unwrap_or(file);
                let rel_str = rel_path.display().to_string();

                // Find routes: e.g. .route("/...", get(...)), @app.get("/..."), app.post("/..."), Router::new()
                for line in content.lines() {
                    let trimmed = line.trim();
                    if (trimmed.contains(".route(") || trimmed.contains("app.get(") || trimmed.contains("app.post(")
                        || trimmed.contains("@app.get(") || trimmed.contains("@app.post(") || trimmed.contains("router.get("))
                        && (trimmed.contains('"') || trimmed.contains('\''))
                    {
                        if let Some(route_path) = extract_route_string(trimmed) {
                            if discovered_routes.insert(route_path.clone()) {
                                let route_id = format!("route_{}", sanitize_id(&route_path));
                                blueprint.nodes.push(ComponentNode {
                                    id: route_id.clone(),
                                    name: route_path.clone(),
                                    kind: ComponentKind::ApiRoute,
                                    path: file.clone(),
                                    description: format!("HTTP Endpoint in {}", rel_str),
                                    metadata: HashMap::new(),
                                });

                                // Link to parent crate if found
                                if let Some(parent_crate) = blueprint.find_enclosing_crate(file) {
                                    blueprint.edges.push(DependencyEdge {
                                        source_id: parent_crate.id.clone(),
                                        target_id: route_id,
                                        label: "exposes".to_string(),
                                    });
                                }
                            }
                        }
                    }

                    // Find database entities: e.g. struct ... { ... }, table! { ... }, model ...
                    if (trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ")) && (trimmed.ends_with('{') || trimmed.contains('{')) {
                        let parts: Vec<&str> = trimmed.split_whitespace().collect();
                        if parts.len() >= 3 {
                            let struct_name = parts[if parts[0] == "pub" { 2 } else { 1 }].trim_matches('{').trim();
                            // Common entity indicators: contains Id, User, Order, Item, Token, Config, Session, Record, Model
                            if (struct_name.ends_with("Model") || struct_name.ends_with("Entity") || struct_name.ends_with("Record")
                                || struct_name == "User" || struct_name == "Account" || struct_name == "Tenant"
                                || struct_name == "Session" || struct_name == "Incident" || struct_name == "SeedRecord")
                                && discovered_entities.insert(struct_name.to_string())
                            {
                                let entity_id = format!("entity_{}", sanitize_id(struct_name));
                                blueprint.nodes.push(ComponentNode {
                                    id: entity_id.clone(),
                                    name: struct_name.to_string(),
                                    kind: ComponentKind::DatabaseEntity,
                                    path: file.clone(),
                                    description: format!("Data model in {}", rel_str),
                                    metadata: HashMap::new(),
                                });

                                if let Some(parent_crate) = blueprint.find_enclosing_crate(file) {
                                    blueprint.edges.push(DependencyEdge {
                                        source_id: parent_crate.id.clone(),
                                        target_id: entity_id,
                                        label: "manages".to_string(),
                                    });
                                }
                            }
                        }
                    }

                    // Find external service connections: reqwest, redis, postgres, ollama, gemini
                    if trimmed.contains("reqwest::Client") || trimmed.contains("redis::") || trimmed.contains("tokio_postgres")
                        || trimmed.contains("OllamaProvider") || trimmed.contains("GeminiProvider")
                    {
                        let svc_name = if trimmed.contains("redis") {
                            "Redis Cache"
                        } else if trimmed.contains("postgres") {
                            "PostgreSQL"
                        } else if trimmed.contains("OllamaProvider") || trimmed.contains("11434") {
                            "Ollama Engine"
                        } else if trimmed.contains("GeminiProvider") {
                            "Gemini API"
                        } else {
                            "External HTTP API"
                        };

                        let svc_id = format!("svc_{}", sanitize_id(svc_name));
                        if !blueprint.nodes.iter().any(|n| n.id == svc_id) {
                            blueprint.nodes.push(ComponentNode {
                                id: svc_id.clone(),
                                name: svc_name.to_string(),
                                kind: ComponentKind::ExternalService,
                                path: file.clone(),
                                description: format!("External dependency accessed via {}", rel_str),
                                metadata: HashMap::new(),
                            });
                        }

                        if let Some(parent_crate) = blueprint.find_enclosing_crate(file) {
                            if !blueprint.edges.iter().any(|e| e.source_id == parent_crate.id && e.target_id == svc_id) {
                                blueprint.edges.push(DependencyEdge {
                                    source_id: parent_crate.id.clone(),
                                    target_id: svc_id,
                                    label: "calls".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 3. Connect crates between themselves if dependencies exist in Cargo.toml
        let crate_nodes: Vec<(String, PathBuf)> = blueprint.nodes.iter()
            .filter(|n| n.kind == ComponentKind::Crate)
            .map(|n| (n.id.clone(), n.path.clone()))
            .collect();

        for (crate_id, crate_path) in &crate_nodes {
            let cargo_path = crate_path.join("Cargo.toml");
            if let Ok(manifest) = fs::read_to_string(&cargo_path) {
                for (other_id, other_path) in &crate_nodes {
                    if crate_id != other_id {
                        let other_name = other_path.file_name().unwrap_or_default().to_string_lossy();
                        if manifest.contains(&format!("{} =", other_name)) || manifest.contains(&format!("\"{}\"", other_name)) {
                            blueprint.edges.push(DependencyEdge {
                                source_id: crate_id.clone(),
                                target_id: other_id.clone(),
                                label: "depends on".to_string(),
                            });
                        }
                    }
                }
            }
        }

        blueprint.total_files_scanned = scanned_files;
        blueprint.total_routes = discovered_routes.len();
        blueprint.total_entities = discovered_entities.len();

        Ok(blueprint)
    }

    /// Finds the enclosing crate for a given source file.
    pub fn find_enclosing_crate(&self, file_path: &Path) -> Option<&ComponentNode> {
        self.nodes.iter()
            .filter(|n| n.kind == ComponentKind::Crate)
            .find(|n| file_path.starts_with(&n.path))
    }

    /// Finds a node by id.
    pub fn find_node(&self, id: &str) -> Option<&ComponentNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Generates valid Mermaid diagram representation.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::new();
        out.push_str("```mermaid\nflowchart TB\n");
        out.push_str("    %% Living Architecture Blueprint generated by Hagibis\n");

        // Crates Subgraph
        let crates: Vec<&ComponentNode> = self.nodes.iter().filter(|n| n.kind == ComponentKind::Crate).collect();
        if !crates.is_empty() {
            out.push_str("    subgraph Crates [Core Workspace Crates]\n");
            for node in crates {
                out.push_str(&format!("        {}[[\"📦 {}\"]]\n", node.id, node.name));
            }
            out.push_str("    end\n\n");
        }

        // Entities Subgraph
        let entities: Vec<&ComponentNode> = self.nodes.iter().filter(|n| n.kind == ComponentKind::DatabaseEntity).collect();
        if !entities.is_empty() {
            out.push_str("    subgraph DataModels [Domain & Data Models]\n");
            for node in entities {
                out.push_str(&format!("        {}[(\"💾 {}\")]\n", node.id, node.name));
            }
            out.push_str("    end\n\n");
        }

        // Routes Subgraph
        let routes: Vec<&ComponentNode> = self.nodes.iter().filter(|n| n.kind == ComponentKind::ApiRoute).collect();
        if !routes.is_empty() {
            out.push_str("    subgraph Endpoints [API & Service Endpoints]\n");
            for node in routes {
                out.push_str(&format!("        {}[\"🌐 {}\"]\n", node.id, node.name));
            }
            out.push_str("    end\n\n");
        }

        // External Services
        let services: Vec<&ComponentNode> = self.nodes.iter().filter(|n| n.kind == ComponentKind::ExternalService).collect();
        if !services.is_empty() {
            out.push_str("    subgraph External [External Services & Integrations]\n");
            for node in services {
                out.push_str(&format!("        {}{{\"⚡ {}\"}}\n", node.id, node.name));
            }
            out.push_str("    end\n\n");
        }

        // Edges
        out.push_str("    %% Component Relationships\n");
        for edge in &self.edges {
            out.push_str(&format!("    {} -->|\"{}\"| {}\n", edge.source_id, edge.label, edge.target_id));
        }

        out.push_str("```\n");
        out
    }

    /// Generates clean ASCII tree diagram for fast terminal viewing.
    pub fn to_ascii(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("┌─ LIVING ARCHITECTURE BLUEPRINT: {} ──────────────\n", self.title));
        out.push_str(&format!("│ Workspace Root: {}\n", self.workspace_root.display()));
        out.push_str(&format!("│ Files Scanned: {} | Components: {} | Edges: {}\n", self.total_files_scanned, self.nodes.len(), self.edges.len()));
        out.push_str("├────────────────────────────────────────────────────────────\n");

        for kind in &[
            ComponentKind::Crate,
            ComponentKind::DatabaseEntity,
            ComponentKind::ApiRoute,
            ComponentKind::ExternalService,
        ] {
            let matching: Vec<&ComponentNode> = self.nodes.iter().filter(|n| &n.kind == kind).collect();
            if !matching.is_empty() {
                out.push_str(&format!("│ {} ({})\n", kind.badge(), matching.len()));
                for node in matching {
                    let connected_edges: Vec<&DependencyEdge> = self.edges.iter()
                        .filter(|e| e.source_id == node.id || e.target_id == node.id)
                        .collect();
                    out.push_str(&format!("│   ├── {}  [{}]\n", node.name, node.path.file_name().unwrap_or_default().to_string_lossy()));
                    for edge in connected_edges.iter().take(3) {
                        if edge.source_id == node.id {
                            out.push_str(&format!("│   │   └──> {} ({})\n", edge.target_id, edge.label));
                        }
                    }
                }
            }
        }
        out.push_str("└────────────────────────────────────────────────────────────\n");
        out
    }

    /// Exports living blueprint to `BLUEPRINT.md` markdown format.
    pub fn export_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str(&format!("# {}\n\n", self.title));
        md.push_str(&format!("> **Living Blueprint Auto-Generated by Hagibis**  \n"));
        md.push_str(&format!("> Scanned `{}` files | Discovered `{}` components and `{}` dependencies.\n\n", self.total_files_scanned, self.nodes.len(), self.edges.len()));

        md.push_str("## 📐 Topology Visualizer\n\n");
        md.push_str(&self.to_mermaid());
        md.push_str("\n\n");

        md.push_str("## 📦 Component Inventory\n\n");
        md.push_str("| Component | Type | Location | Description |\n");
        md.push_str("|-----------|------|----------|-------------|\n");
        for node in &self.nodes {
            let rel = node.path.strip_prefix(&self.workspace_root).unwrap_or(&node.path);
            md.push_str(&format!("| `{}` | {} | `{}` | {} |\n", node.name, node.kind.badge(), rel.display(), node.description));
        }

        md.push_str("\n## 🔗 Inter-Component Dependencies\n\n");
        md.push_str("| Source | Relationship | Target |\n");
        md.push_str("|--------|--------------|--------|\n");
        for edge in &self.edges {
            md.push_str(&format!("| `{}` | *{}* | `{}` |\n", edge.source_id, edge.label, edge.target_id));
        }

        md
    }

    /// Writes the markdown representation to disk.
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let content = self.export_markdown();
        fs::write(path, content)?;
        Ok(())
    }
}

fn collect_source_files(dir: &Path, files: &mut Vec<PathBuf>, depth: usize, max_depth: usize) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            if file_name.starts_with('.') || file_name == "target" || file_name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                collect_source_files(&path, files, depth + 1, max_depth);
            } else if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go") {
                    files.push(path);
                }
            }
        }
    }
}

fn extract_route_string(line: &str) -> Option<String> {
    if let Some(start) = line.find('"') {
        if let Some(end) = line[start + 1..].find('"') {
            let val = &line[start + 1..start + 1 + end];
            if val.starts_with('/') {
                return Some(val.to_string());
            }
        }
    }
    if let Some(start) = line.find('\'') {
        if let Some(end) = line[start + 1..].find('\'') {
            let val = &line[start + 1..start + 1 + end];
            if val.starts_with('/') {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}
