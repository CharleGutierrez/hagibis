//! # Folder & Network Share Inspection and Auto-Grounding Engine
//!
//! Provides zero-mock POSIX directory tree inspection, network share probing (CIFS/SMB/NFS),
//! active LAN computer discovery via ARP/hosts, storage drive parsing, and prompt auto-grounding.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Detected target category
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    LocalDirectory(PathBuf),
    NetworkUri(String),
    SystemStorage,
    NetworkDiscovery,
    UserXdgDirectories,
}

/// High-signal file type categories
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileTypeKind {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    C,
    Cpp,
    Shell,
    HtmlCss,
    Json,
    Yaml,
    Toml,
    Markdown,
    Sql,
    Text,
    Image,
    Archive,
    Binary,
    Directory,
    Symlink,
    Unknown,
}

impl FileTypeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Shell => "Shell",
            Self::HtmlCss => "HTML/CSS",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Markdown => "Markdown",
            Self::Sql => "SQL",
            Self::Text => "Text",
            Self::Image => "Image",
            Self::Archive => "Archive",
            Self::Binary => "Binary",
            Self::Directory => "Directory",
            Self::Symlink => "Symlink",
            Self::Unknown => "Unknown",
        }
    }
}

/// Metadata and classification for an inspected node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNodeInfo {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_target: Option<PathBuf>,
    pub size_bytes: u64,
    pub permissions_mode: u32,
    pub file_type: FileTypeKind,
    pub modified_timestamp: Option<u64>,
}

/// Summary metrics for an inspected directory
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DirectoryTreeSummary {
    pub total_files: usize,
    pub total_dirs: usize,
    pub total_bytes: u64,
    pub extension_counts: HashMap<String, usize>,
    pub truncated: bool,
}

/// Discovered network share
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkShareInfo {
    pub device_or_remote: String,
    pub mount_point: PathBuf,
    pub fstype: String,
    pub options: String,
    pub is_active: bool,
}

/// Discovered network computer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkComputerInfo {
    pub ip_address: String,
    pub hostname: Option<String>,
    pub mac_address: Option<String>,
    pub interface: Option<String>,
    pub smb_open: Option<bool>,
    pub nfs_open: Option<bool>,
}

/// Discovered local storage partition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDriveInfo {
    pub device: String,
    pub mount_point: PathBuf,
    pub fstype: String,
    pub options: String,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

/// Inspected target report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectedTargetReport {
    pub target: TargetKind,
    pub display_title: String,
    pub summary_line: String,
    pub tree_rendered: Option<String>,
    pub storage_drives: Vec<StorageDriveInfo>,
    pub network_shares: Vec<NetworkShareInfo>,
    pub network_computers: Vec<NetworkComputerInfo>,
    pub estimated_tokens: usize,
}

/// Metric details for a file or directory node in size profiling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntryMetric {
    pub path: PathBuf,
    pub name: String,
    pub size_bytes: u64,
    pub is_dir: bool,
}

/// Detected build artifact or cache bloat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildBloatItem {
    pub path: PathBuf,
    pub name: String,
    pub size_bytes: u64,
    pub recommendation: String,
}

/// Security vulnerability or permission hygiene flaw detected in files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityWarning {
    pub path: PathBuf,
    pub issue: String,
    pub severity: String,
    pub recommendation: String,
}

/// Deep architectural analysis report for a directory or codebase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderAnalysisReport {
    pub path: PathBuf,
    pub archetype: String,
    pub archetype_traits: Vec<String>,
    pub total_files: usize,
    pub total_dirs: usize,
    pub total_bytes: u64,
    pub largest_items: Vec<FileEntryMetric>,
    pub bloat_items: Vec<BuildBloatItem>,
    pub total_bloat_bytes: u64,
    pub hygiene_score: u32,
    pub hygiene_findings: Vec<String>,
    pub security_warnings: Vec<SecurityWarning>,
}

/// Deep analysis of a mounted storage partition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDriveAnalysis {
    pub device: String,
    pub mount_point: PathBuf,
    pub fstype: String,
    pub options: String,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub usage_percent: Option<f64>,
    pub is_critical: bool,
    pub warning: Option<String>,
    pub filesystem_reliability: String,
    pub mount_flags: Vec<String>,
    pub security_audit: Vec<String>,
}

/// Deep storage saturation, reliability, and security report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageAnalysisReport {
    pub drives: Vec<StorageDriveAnalysis>,
    pub total_capacity_bytes: u64,
    pub total_available_bytes: u64,
    pub overall_usage_percent: f64,
    pub critical_partitions: Vec<String>,
    pub system_warnings: Vec<String>,
    pub recommendations: Vec<String>,
}

/// Deep analysis of an active network host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkHostAnalysis {
    pub ip_address: String,
    pub hostname: Option<String>,
    pub mac_address: Option<String>,
    pub interface: Option<String>,
    pub category: String,
    pub is_gateway: bool,
    pub smb_open: Option<bool>,
    pub nfs_open: Option<bool>,
    pub notes: Vec<String>,
}

/// Deep network infrastructure and subnet topology report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkAnalysisReport {
    pub detected_gateways: Vec<String>,
    pub active_hosts: Vec<NetworkHostAnalysis>,
    pub host_classification_summary: HashMap<String, usize>,
    pub mounted_shares: Vec<NetworkShareInfo>,
    pub reachability_assessment: Vec<String>,
    pub security_considerations: Vec<String>,
}

/// Comprehensive executive diagnosis report wrapping folder, storage, and network
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComprehensiveSystemAnalysisReport {
    pub folder_analysis: Option<FolderAnalysisReport>,
    pub storage_analysis: Option<StorageAnalysisReport>,
    pub network_analysis: Option<NetworkAnalysisReport>,
    pub executive_diagnosis: String,
    pub health_verdict: String,
    pub actionable_recommendations: Vec<String>,
}

/// Configuration settings
#[derive(Debug, Clone)]
pub struct FolderInspectionConfig {
    pub max_depth: usize,
    pub max_entries_per_dir: usize,
    pub max_total_entries: usize,
    pub show_hidden: bool,
    pub probe_network_ports: bool,
    pub probe_timeout_ms: u64,
}

impl Default for FolderInspectionConfig {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_entries_per_dir: 40,
            max_total_entries: 250,
            show_hidden: false,
            probe_network_ports: false,
            probe_timeout_ms: 350,
        }
    }
}

pub struct FolderInspectorEngine;

impl FolderInspectorEngine {
    /// Detect target paths and implicit intent from natural language prompts,
    /// supporting folder names and paths with spaces (quoted or unquoted, e.g. "My AI Project").
    pub fn detect_targets(workspace_root: &Path, prompt: &str) -> Vec<TargetKind> {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();
        let home_dir = std::env::var("HOME").ok().map(PathBuf::from);
        let cwd_opt = std::env::current_dir().ok();

        // 1. Network URIs (smb://, nfs://, cifs://, file://, \\server\share)
        let net_uri_re = Regex::new(r#"(?i)\b(smb|cifs|nfs|nfs4|file)://[^\s"'<>]+"#).unwrap();
        for caps in net_uri_re.captures_iter(prompt) {
            let uri = caps[0].to_string();
            if seen.insert(uri.clone()) {
                targets.push(TargetKind::NetworkUri(uri));
            }
        }

        let unc_re = Regex::new(r#"\\\\[a-zA-Z0-9_\.\-]+(\\[a-zA-Z0-9_\.\-]+)+"#).unwrap();
        for caps in unc_re.captures_iter(prompt) {
            let uri = caps[0].to_string();
            if seen.insert(uri.clone()) {
                targets.push(TargetKind::NetworkUri(uri));
            }
        }

        // 2. Explicit Quoted Strings (supports folders and paths with spaces like "My AI Project" or '~/My AI Project')
        let quoted_re = Regex::new(r#"["'`]{1}([^"'`\r\n]+)["'`]{1}"#).unwrap();
        for caps in quoted_re.captures_iter(prompt) {
            let raw = caps[1].trim();
            if raw.is_empty() {
                continue;
            }

            // Check if quoted string is an absolute path or tilde path
            let expanded_path = if let Some(stripped) = raw.strip_prefix("~/") {
                home_dir.as_ref().map(|h| h.join(stripped))
            } else if raw == "~" {
                home_dir.clone()
            } else if raw.starts_with('/') {
                Some(PathBuf::from(raw))
            } else {
                None
            };

            if let Some(ref p) = expanded_path {
                if (p.is_dir() || Self::is_standard_system_dir(p)) && seen.insert(p.to_string_lossy().to_string()) {
                    targets.push(TargetKind::LocalDirectory(p.clone()));
                    continue;
                }
            }

            // Check relative to workspace root
            let ws_cand = workspace_root.join(raw);
            if ws_cand.is_dir() && seen.insert(ws_cand.to_string_lossy().to_string()) {
                targets.push(TargetKind::LocalDirectory(ws_cand));
                continue;
            }

            // Check relative to home directory
            if let Some(ref h) = home_dir {
                let home_cand = h.join(raw);
                if home_cand.is_dir() && seen.insert(home_cand.to_string_lossy().to_string()) {
                    targets.push(TargetKind::LocalDirectory(home_cand));
                    continue;
                }
            }

            // Check relative to current working directory
            if let Some(ref cwd) = cwd_opt {
                let cwd_cand = cwd.join(raw);
                if cwd_cand.is_dir() && seen.insert(cwd_cand.to_string_lossy().to_string()) {
                    targets.push(TargetKind::LocalDirectory(cwd_cand));
                    continue;
                }
            }
        }

        // 3. Absolute POSIX paths (/...) with greedy lookahead for spaces (e.g. /home/toshiba/My AI Project)
        let abs_start_re = Regex::new(r#"(?:^|[\s"'`\(\[\{])(/[^"'`\(\[\{\r\n]+)"#).unwrap();
        for caps in abs_start_re.captures_iter(prompt) {
            let raw_segment = caps[1].trim();
            let words: Vec<&str> = raw_segment.split_whitespace().collect();
            for k in (1..=words.len().min(8)).rev() {
                let candidate_str = words[..k].join(" ");
                let clean_cand = candidate_str.trim_matches(|c| c == ',' || c == ';' || c == ':' || c == '.' || c == '"' || c == '\'' || c == '`' || c == ')' || c == ']');
                let p = PathBuf::from(clean_cand);
                let is_valid = p.is_dir() || (k == 1 && clean_cand.len() > 1 && Self::is_standard_system_dir(&p));
                if is_valid {
                    if seen.insert(p.to_string_lossy().to_string()) {
                        targets.push(TargetKind::LocalDirectory(p));
                        break;
                    }
                }
            }
        }

        // 4. Tilde paths (~/...) with greedy lookahead for spaces (e.g. ~/My AI Project)
        let tilde_start_re = Regex::new(r#"(?:^|[\s"'`\(\[\{])(~[^"'`\(\[\{\r\n]*)"#).unwrap();
        if let Some(ref home) = home_dir {
            for caps in tilde_start_re.captures_iter(prompt) {
                let raw_segment = caps[1].trim();
                let words: Vec<&str> = raw_segment.split_whitespace().collect();
                for k in (1..=words.len().min(8)).rev() {
                    let candidate_str = words[..k].join(" ");
                    let clean_cand = candidate_str.trim_matches(|c| c == ',' || c == ';' || c == ':' || c == '.' || c == '"' || c == '\'' || c == '`' || c == ')' || c == ']');
                    let expanded = if clean_cand == "~" {
                        home.clone()
                    } else if let Some(stripped) = clean_cand.strip_prefix("~/") {
                        home.join(stripped)
                    } else {
                        continue;
                    };
                    if expanded.is_dir() && seen.insert(expanded.to_string_lossy().to_string()) {
                        targets.push(TargetKind::LocalDirectory(expanded));
                        break;
                    }
                }
            }
        }

        // 5. Natural language trigger words: "folder <name>", "directory <name>", "dir <name>", "path <name>", "in <name>"
        let trigger_re = Regex::new(r#"(?i)\b(?:folder|directory|dir|path|under|in)\s+([^,\r\n\.;\?!]+)"#).unwrap();
        for caps in trigger_re.captures_iter(prompt) {
            let after_trigger = caps[1].trim();
            let words: Vec<&str> = after_trigger.split_whitespace().collect();
            for k in (1..=words.len().min(6)).rev() {
                let raw_cand = words[..k].join(" ");
                let clean_cand = raw_cand.trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '(' || c == ')' || c == '[' || c == ']');
                if clean_cand.is_empty() {
                    continue;
                }

                let ws_cand = workspace_root.join(clean_cand);
                if ws_cand.is_dir() && seen.insert(ws_cand.to_string_lossy().to_string()) {
                    targets.push(TargetKind::LocalDirectory(ws_cand));
                    break;
                }

                if let Some(ref h) = home_dir {
                    let home_cand = h.join(clean_cand);
                    if home_cand.is_dir() && seen.insert(home_cand.to_string_lossy().to_string()) {
                        targets.push(TargetKind::LocalDirectory(home_cand));
                        break;
                    }
                }

                if let Some(ref cwd) = cwd_opt {
                    let cwd_cand = cwd.join(clean_cand);
                    if cwd_cand.is_dir() && seen.insert(cwd_cand.to_string_lossy().to_string()) {
                        targets.push(TargetKind::LocalDirectory(cwd_cand));
                        break;
                    }
                }
            }
        }

        // 6. Direct scan for existing directories with spaces in workspace root, home dir, and cwd
        // If a directory contains spaces (e.g. "My AI Project") and the prompt mentions its name,
        // automatically inspect it!
        let mut scan_parents = vec![workspace_root.to_path_buf()];
        if let Some(ref h) = home_dir {
            if h != workspace_root {
                scan_parents.push(h.clone());
            }
        }
        if let Some(ref cwd) = cwd_opt {
            if cwd != workspace_root && Some(cwd) != home_dir.as_ref() {
                scan_parents.push(cwd.clone());
            }
        }

        let prompt_lower = prompt.to_lowercase();
        for parent in scan_parents {
            if let Ok(entries) = fs::read_dir(&parent) {
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type() {
                        if ft.is_dir() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if name.contains(' ') && prompt_lower.contains(&name.to_lowercase()) {
                                let dir_path = entry.path();
                                if seen.insert(dir_path.to_string_lossy().to_string()) {
                                    targets.push(TargetKind::LocalDirectory(dir_path));
                                }
                            }
                        }
                    }
                }
            }
        }

        // 7. Relative paths (./..., ../..., or relative to workspace)
        let rel_re = Regex::new(r#"(?:^|[\s"'`\(\[\{])((\.{1,2}/|[a-zA-Z0-9_\-]+/)[a-zA-Z0-9_\.\-/]*)"#).unwrap();
        for caps in rel_re.captures_iter(prompt) {
            let raw = caps[1].trim_matches(|c| c == '\'' || c == '"' || c == '`');
            let candidate = workspace_root.join(raw);
            if candidate.is_dir() && seen.insert(candidate.to_string_lossy().to_string()) {
                targets.push(TargetKind::LocalDirectory(candidate));
            }
        }

        // 5. Implicit triggers (Natural Language Intent)
        let lower = prompt.to_lowercase();
        let mentions_storage = lower.contains("storage drives")
            || lower.contains("what disks")
            || lower.contains("list storage")
            || lower.contains("storage")
            || lower.contains("folders available on this computer")
            || lower.contains("folders available in the computer")
            || lower.contains("folders are on this computer")
            || lower.contains("folders available in this computer")
            || (lower.contains("folder") && (lower.contains("computer") || lower.contains("system") || lower.contains("machine")))
            || lower.contains("list partitions")
            || lower.contains("partitions")
            || lower.contains("mounted filesystems")
            || lower.contains("disk space")
            || lower.contains("disks");

        if mentions_storage && seen.insert("intent:storage".to_string()) {
            targets.push(TargetKind::SystemStorage);
        }

        let mentions_network = lower.contains("scan network")
            || lower.contains("network shares")
            || lower.contains("network computers")
            || lower.contains("network of the computers")
            || lower.contains("local network")
            || lower.contains("lan computers")
            || lower.contains("lan hosts")
            || lower.contains("arp")
            || (lower.contains("network") && (
                lower.contains("computer")
                || lower.contains("ip")
                || lower.contains("host")
                || lower.contains("share")
                || lower.contains("device")
                || lower.contains("folder")
                || lower.contains("drive")
                || lower.contains("available")
            ))
            || lower.contains("discover shares")
            || lower.contains("smb shares")
            || lower.contains("nfs mounts");

        if mentions_network && seen.insert("intent:network".to_string()) {
            targets.push(TargetKind::NetworkDiscovery);
        }

        if lower.contains("user folders")
            || lower.contains("user directories")
            || lower.contains("xdg directories")
            || lower.contains("folders in my home")
            || lower.contains("folders in my computer")
        {
            if seen.insert("intent:xdg".to_string()) {
                targets.push(TargetKind::UserXdgDirectories);
            }
        }

        targets
    }

    /// Check if path is a standard Linux system directory
    fn is_standard_system_dir(p: &Path) -> bool {
        let s = p.to_string_lossy();
        s.starts_with("/home")
            || s.starts_with("/etc")
            || s.starts_with("/var")
            || s.starts_with("/tmp")
            || s.starts_with("/mnt")
            || s.starts_with("/media")
            || s.starts_with("/opt")
            || s.starts_with("/srv")
            || s.starts_with("/usr")
            || s.starts_with("/run")
    }

    /// Check security guardrail: sensitive secret file or directory
    pub fn is_sensitive_target(p: &Path) -> bool {
        let s = p.to_string_lossy().to_lowercase();
        s.contains("/etc/shadow")
            || s.contains("/etc/gshadow")
            || s.contains("/etc/sudoers")
            || s.contains("/.ssh/id_")
            || s.ends_with(".key")
            || s.ends_with(".pem")
            || s.ends_with(".pfx")
            || s.ends_with(".env")
            || s.ends_with(".env.production")
    }

    /// Inspect a directory and generate a structured ASCII tree with metadata
    pub fn inspect_directory(
        dir_path: &Path,
        config: &FolderInspectionConfig,
    ) -> Result<(String, DirectoryTreeSummary)> {
        if Self::is_sensitive_target(dir_path) {
            return Err(HgbError::Security(format!(
                "Access to sensitive target blocked: {}",
                dir_path.display()
            )));
        }

        let mut summary = DirectoryTreeSummary::default();
        let mut visited_inodes = HashSet::new();
        let mut lines = Vec::new();

        lines.push(format!("📁 {}", dir_path.display()));
        Self::walk_dir_recursive(
            dir_path,
            "",
            0,
            config,
            &mut summary,
            &mut visited_inodes,
            &mut lines,
        )?;

        Ok((lines.join("\n"), summary))
    }

    fn walk_dir_recursive(
        current_dir: &Path,
        prefix: &str,
        depth: usize,
        config: &FolderInspectionConfig,
        summary: &mut DirectoryTreeSummary,
        visited_inodes: &mut HashSet<(u64, u64)>,
        output: &mut Vec<String>,
    ) -> Result<()> {
        if depth >= config.max_depth || summary.total_files + summary.total_dirs >= config.max_total_entries {
            summary.truncated = true;
            return Ok(());
        }

        #[cfg(unix)]
        {
            if let Ok(meta) = fs::metadata(current_dir) {
                let key = (meta.dev(), meta.ino());
                if !visited_inodes.insert(key) {
                    output.push(format!("{prefix}└── ⚠️ [CIRCULAR SYMLINK LOOP DETECTED]"));
                    return Ok(());
                }
            }
        }

        let read_dir = match fs::read_dir(current_dir) {
            Ok(rd) => rd,
            Err(e) => {
                output.push(format!("{prefix}└── ⚠️ [Permission Denied / Error: {e}]"));
                return Ok(());
            }
        };

        let mut entries = Vec::new();
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !config.show_hidden && name.starts_with('.') {
                continue;
            }
            if Self::is_sensitive_target(&entry.path()) {
                continue;
            }
            entries.push(entry);
        }

        // Sort: directories first, then alphabetical
        entries.sort_by(|a, b| {
            let a_is_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_is_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.file_name().cmp(&b.file_name()),
            }
        });

        let total_count = entries.len();
        let display_limit = config.max_entries_per_dir.min(total_count);

        for (i, entry) in entries.iter().take(display_limit).enumerate() {
            let is_last = i + 1 == display_limit && display_limit == total_count;
            let branch = if is_last { "└── " } else { "├── " };
            let next_prefix = if is_last { format!("{prefix}    ") } else { format!("{prefix}│   ") };

            let path = entry.path();
            let symlink_meta = fs::symlink_metadata(&path);
            let is_symlink = symlink_meta.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
            let symlink_target = if is_symlink { fs::read_link(&path).ok() } else { None };

            let meta = fs::metadata(&path).ok().or_else(|| symlink_meta.ok());
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

            #[cfg(unix)]
            let mode = meta.as_ref().map(|m| m.mode()).unwrap_or(0);
            #[cfg(not(unix))]
            let mode = 0u32;

            let file_type = if is_dir {
                FileTypeKind::Directory
            } else if is_symlink {
                FileTypeKind::Symlink
            } else {
                Self::classify_file(&path)
            };

            let name = entry.file_name().to_string_lossy().to_string();

            if is_dir {
                summary.total_dirs += 1;
                output.push(format!("{prefix}{branch}📁 {name}/"));
                Self::walk_dir_recursive(&path, &next_prefix, depth + 1, config, summary, visited_inodes, output)?;
            } else {
                summary.total_files += 1;
                summary.total_bytes += size;
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    *summary.extension_counts.entry(ext.to_lowercase()).or_insert(0) += 1;
                }

                let size_str = Self::format_bytes(size);
                let perm_str = Self::format_mode(mode);
                let link_suffix = symlink_target
                    .map(|tgt| format!(" -> {}", tgt.display()))
                    .unwrap_or_default();

                output.push(format!(
                    "{prefix}{branch}{name}{link_suffix} ({size_str}) [{}] [{perm_str}]",
                    file_type.as_str()
                ));
            }
        }

        if total_count > display_limit {
            output.push(format!("{prefix}└── ... and {} more entries", total_count - display_limit));
            summary.truncated = true;
        }

        Ok(())
    }

    /// Detect file type using extension and binary magic bytes
    pub fn classify_file(path: &Path) -> FileTypeKind {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            match ext.to_lowercase().as_str() {
                "rs" => return FileTypeKind::Rust,
                "ts" | "tsx" => return FileTypeKind::TypeScript,
                "js" | "jsx" | "mjs" => return FileTypeKind::JavaScript,
                "py" | "pyw" => return FileTypeKind::Python,
                "go" => return FileTypeKind::Go,
                "c" | "h" => return FileTypeKind::C,
                "cpp" | "hpp" | "cc" | "cxx" => return FileTypeKind::Cpp,
                "sh" | "bash" | "zsh" => return FileTypeKind::Shell,
                "html" | "htm" | "css" | "scss" | "sass" => return FileTypeKind::HtmlCss,
                "json" => return FileTypeKind::Json,
                "yaml" | "yml" => return FileTypeKind::Yaml,
                "toml" => return FileTypeKind::Toml,
                "md" | "markdown" => return FileTypeKind::Markdown,
                "sql" => return FileTypeKind::Sql,
                "txt" | "log" | "cfg" | "ini" | "conf" => return FileTypeKind::Text,
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => return FileTypeKind::Image,
                "tar" | "gz" | "zip" | "bz2" | "xz" | "7z" => return FileTypeKind::Archive,
                "so" | "bin" | "exe" | "o" | "a" => return FileTypeKind::Binary,
                _ => {}
            }
        }

        // Sniff first 512 bytes for binary null bytes or ELF
        if let Ok(mut f) = File::open(path) {
            let mut buf = [0u8; 512];
            if let Ok(n) = f.read(&mut buf) {
                let slice = &buf[..n];
                if slice.starts_with(b"\x7fELF") || slice.iter().any(|&b| b == 0) {
                    return FileTypeKind::Binary;
                }
                return FileTypeKind::Text;
            }
        }

        FileTypeKind::Unknown
    }

    /// Query filesystem total and available space via POSIX statvfs
    #[cfg(unix)]
    pub fn get_mount_space(path: &Path) -> (Option<u64>, Option<u64>) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        if let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) {
            unsafe {
                let mut stat: libc::statvfs = std::mem::zeroed();
                if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                    let block_size = if stat.f_frsize != 0 {
                        stat.f_frsize as u64
                    } else {
                        stat.f_bsize as u64
                    };
                    let total = stat.f_blocks as u64 * block_size;
                    let available = stat.f_bavail as u64 * block_size;
                    return (Some(total), Some(available));
                }
            }
        }
        (None, None)
    }

    #[cfg(not(unix))]
    pub fn get_mount_space(_path: &Path) -> (Option<u64>, Option<u64>) {
        (None, None)
    }

    /// Parse /proc/mounts and /etc/fstab for storage partitions
    pub fn discover_storage_drives() -> Vec<StorageDriveInfo> {
        let mut drives = Vec::new();
        let mounts_file = Path::new("/proc/mounts");
        if !mounts_file.exists() {
            return drives;
        }

        if let Ok(file) = File::open(mounts_file) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let dev = parts[0];
                    let mount_point = PathBuf::from(parts[1]);
                    let fstype = parts[2];
                    let options = parts[3];

                    let is_storage = (dev.starts_with("/dev/") && !dev.starts_with("/dev/loop"))
                        || matches!(
                            fstype,
                            "ext4" | "ext3" | "ext2" | "btrfs" | "xfs" | "zfs" | "vfat" | "f2fs" | "ntfs" | "exfat"
                        );

                    if is_storage {
                        let (total_bytes, available_bytes) = Self::get_mount_space(&mount_point);
                        drives.push(StorageDriveInfo {
                            device: dev.to_string(),
                            mount_point,
                            fstype: fstype.to_string(),
                            options: options.to_string(),
                            total_bytes,
                            available_bytes,
                        });
                    }
                }
            }
        }

        drives
    }

    /// Discover network shares from /proc/mounts (cifs, smb, nfs, sshfs, 9p, gvfs)
    pub fn discover_network_shares() -> Vec<NetworkShareInfo> {
        let mut shares = Vec::new();
        let mounts_file = Path::new("/proc/mounts");
        if !mounts_file.exists() {
            return shares;
        }

        let net_types = [
            "cifs", "smb", "smbfs", "nfs", "nfs4", "sshfs", "fuse.sshfs", "davfs", "9p",
            "fuse.gvfsd-fuse",
        ];

        if let Ok(file) = File::open(mounts_file) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let dev = parts[0];
                    let mount_point = PathBuf::from(parts[1]);
                    let fstype = parts[2];
                    let options = parts[3];

                    if net_types.contains(&fstype) || dev.starts_with("//") {
                        shares.push(NetworkShareInfo {
                            device_or_remote: dev.to_string(),
                            mount_point,
                            fstype: fstype.to_string(),
                            options: options.to_string(),
                            is_active: true,
                        });
                    }
                }
            }
        }

        shares
    }

    /// Discover active local network computers via /proc/net/arp and /etc/hosts
    pub fn discover_network_computers(probe_ports: bool, timeout_ms: u64) -> Vec<NetworkComputerInfo> {
        let mut computers: HashMap<String, NetworkComputerInfo> = HashMap::new();

        // 1. Parse /proc/net/arp
        if let Ok(file) = File::open("/proc/net/arp") {
            let reader = BufReader::new(file);
            for (idx, line) in reader.lines().flatten().enumerate() {
                if idx == 0 {
                    continue; // Skip header
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 6 {
                    let ip = parts[0].to_string();
                    let mac = parts[3].to_string();
                    let iface = parts[5].to_string();

                    // Filter out incomplete MAC entries
                    if mac != "00:00:00:00:00:00" && !ip.starts_with("127.") {
                        computers.insert(
                            ip.clone(),
                            NetworkComputerInfo {
                                ip_address: ip,
                                hostname: None,
                                mac_address: Some(mac),
                                interface: Some(iface),
                                smb_open: None,
                                nfs_open: None,
                            },
                        );
                    }
                }
            }
        }

        // 2. Parse /etc/hosts
        if let Ok(file) = File::open("/etc/hosts") {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let clean = line.split('#').next().unwrap_or("").trim();
                if clean.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = clean.split_whitespace().collect();
                if parts.len() >= 2 {
                    let ip = parts[0].to_string();
                    let hostname = parts[1].to_string();

                    if !ip.starts_with("127.") && !ip.starts_with("::1") {
                        let entry = computers.entry(ip.clone()).or_insert_with(|| NetworkComputerInfo {
                            ip_address: ip,
                            hostname: None,
                            mac_address: None,
                            interface: None,
                            smb_open: None,
                            nfs_open: None,
                        });
                        if entry.hostname.is_none() {
                            entry.hostname = Some(hostname);
                        }
                    }
                }
            }
        }

        let mut results: Vec<NetworkComputerInfo> = computers.into_values().collect();

        // 3. Optional quick TCP port probe for SMB (445) and NFS (2049)
        if probe_ports {
            for comp in &mut results {
                if let Ok(ip_addr) = comp.ip_address.parse::<std::net::IpAddr>() {
                    comp.smb_open = Some(Self::probe_tcp_port(ip_addr, 445, timeout_ms));
                    comp.nfs_open = Some(Self::probe_tcp_port(ip_addr, 2049, timeout_ms));
                }
            }
        }

        results.sort_by(|a, b| a.ip_address.cmp(&b.ip_address));
        results
    }

    /// Non-blocking TCP port connection probe with timeout
    fn probe_tcp_port(ip: std::net::IpAddr, port: u16, timeout_ms: u64) -> bool {
        let socket = SocketAddr::new(ip, port);
        TcpStream::connect_timeout(&socket, Duration::from_millis(timeout_ms)).is_ok()
    }

    /// Parse user XDG directories from ~/.config/user-dirs.dirs
    pub fn discover_xdg_directories() -> Vec<(String, PathBuf)> {
        let mut dirs = Vec::new();
        let home = match std::env::var("HOME") {
            Ok(h) => PathBuf::from(h),
            Err(_) => return dirs,
        };

        let conf_file = home.join(".config/user-dirs.dirs");
        if let Ok(file) = File::open(conf_file) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let clean = line.trim();
                if clean.starts_with('#') || !clean.contains('=') {
                    continue;
                }
                let mut split = clean.splitn(2, '=');
                let key = split.next().unwrap_or("").trim();
                let mut val = split.next().unwrap_or("").trim().trim_matches('"').to_string();

                if val.starts_with("$HOME") {
                    val = val.replacen("$HOME", &home.to_string_lossy(), 1);
                }

                let p = PathBuf::from(&val);
                if p.is_dir() {
                    let label = key.strip_prefix("XDG_").unwrap_or(key).strip_suffix("_DIR").unwrap_or(key);
                    dirs.push((label.to_string(), p));
                }
            }
        } else {
            // Standard fallback folders
            for name in &["Desktop", "Documents", "Downloads", "Pictures", "Music", "Videos"] {
                let p = home.join(name);
                if p.is_dir() {
                    dirs.push((name.to_string(), p));
                }
            }
        }

        dirs
    }

    /// Augment prompt with inspected local and network folder context
    pub fn augment_prompt_with_folders_ctx(
        workspace_root: &Path,
        prompt: &str,
        max_tokens: usize,
    ) -> Result<(String, Vec<InspectedTargetReport>)> {
        let targets = Self::detect_targets(workspace_root, prompt);
        if targets.is_empty() {
            return Ok((prompt.to_string(), Vec::new()));
        }

        let config = FolderInspectionConfig::default();
        let mut reports = Vec::new();
        let mut context_blocks = Vec::new();

        for target in &targets {
            match target {
                TargetKind::LocalDirectory(ref p) => {
                    if let Ok((tree, summary)) = Self::inspect_directory(p, &config) {
                        let summary_line = format!(
                            "{} files, {} directories, {}",
                            summary.total_files,
                            summary.total_dirs,
                            Self::format_bytes(summary.total_bytes)
                        );
                        let token_est = tree.len() / 4;
                        let report = InspectedTargetReport {
                            target: target.clone(),
                            display_title: format!("📁 Local Directory: {}", p.display()),
                            summary_line: summary_line.clone(),
                            tree_rendered: Some(tree.clone()),
                            storage_drives: Vec::new(),
                            network_shares: Vec::new(),
                            network_computers: Vec::new(),
                            estimated_tokens: token_est,
                        };
                        reports.push(report);
                        context_blocks.push(format!(
                            "### Local Directory: {}\nSummary: {}\n```text\n{}\n```\n",
                            p.display(),
                            summary_line,
                            tree
                        ));
                    }
                }
                TargetKind::NetworkUri(ref uri) => {
                    let summary = format!("Inspected network URI endpoint: {uri}");
                    reports.push(InspectedTargetReport {
                        target: target.clone(),
                        display_title: format!("🌐 Network URI: {uri}"),
                        summary_line: summary.clone(),
                        tree_rendered: None,
                        storage_drives: Vec::new(),
                        network_shares: Vec::new(),
                        network_computers: Vec::new(),
                        estimated_tokens: 30,
                    });
                    context_blocks.push(format!("### Network Share Target: {uri}\n{summary}\n"));
                }
                TargetKind::SystemStorage => {
                    let drives = Self::discover_storage_drives();
                    let mut text = String::from("### Mounted Storage Drives & Partitions:\n");
                    for d in &drives {
                        let space_info = match (d.total_bytes, d.available_bytes) {
                            (Some(tot), Some(avail)) => format!(
                                " (Avail: {} / Total: {})",
                                Self::format_bytes(avail),
                                Self::format_bytes(tot)
                            ),
                            _ => String::new(),
                        };
                        text.push_str(&format!(
                            "- Device: `{}` mounted on `{}` [{}] {}\n",
                            d.device,
                            d.mount_point.display(),
                            d.fstype,
                            space_info
                        ));
                    }
                    reports.push(InspectedTargetReport {
                        target: target.clone(),
                        display_title: "💾 System Storage & Drives".to_string(),
                        summary_line: format!("{} storage partitions discovered", drives.len()),
                        tree_rendered: None,
                        storage_drives: drives,
                        network_shares: Vec::new(),
                        network_computers: Vec::new(),
                        estimated_tokens: text.len() / 4,
                    });
                    context_blocks.push(text);
                }
                TargetKind::NetworkDiscovery => {
                    let shares = Self::discover_network_shares();
                    let computers = Self::discover_network_computers(
                        config.probe_network_ports,
                        config.probe_timeout_ms,
                    );

                    let mut text = String::from("### Network Shares & Active Network Computers:\n");
                    if !shares.is_empty() {
                        text.push_str("#### Mounted Shares:\n");
                        for s in &shares {
                            text.push_str(&format!(
                                "- `{}` -> `{}` [{}]\n",
                                s.device_or_remote,
                                s.mount_point.display(),
                                s.fstype
                            ));
                        }
                    }
                    if !computers.is_empty() {
                        text.push_str("#### Active Network Computers:\n");
                        for c in &computers {
                            let smb_status = match c.smb_open {
                                Some(true) => "SMB: Open",
                                Some(false) => "SMB: Closed",
                                None => "SMB: Unprobed",
                            };
                            let host = c.hostname.as_deref().unwrap_or("Unknown Host");
                            text.push_str(&format!(
                                "- IP: `{}` ({}) [{smb_status}]\n",
                                c.ip_address, host
                            ));
                        }
                    }
                    reports.push(InspectedTargetReport {
                        target: target.clone(),
                        display_title: "🌐 Network Shares & Computers".to_string(),
                        summary_line: format!(
                            "{} shares, {} network hosts",
                            shares.len(),
                            computers.len()
                        ),
                        tree_rendered: None,
                        storage_drives: Vec::new(),
                        network_shares: shares,
                        network_computers: computers,
                        estimated_tokens: text.len() / 4,
                    });
                    context_blocks.push(text);
                }
                TargetKind::UserXdgDirectories => {
                    let xdg = Self::discover_xdg_directories();
                    let mut text = String::from("### User Standard Directories (XDG):\n");
                    for (lbl, p) in &xdg {
                        text.push_str(&format!("- **{lbl}**: `{}`\n", p.display()));
                    }
                    reports.push(InspectedTargetReport {
                        target: target.clone(),
                        display_title: "📂 User Standard Folders".to_string(),
                        summary_line: format!("{} user directories located", xdg.len()),
                        tree_rendered: None,
                        storage_drives: Vec::new(),
                        network_shares: Vec::new(),
                        network_computers: Vec::new(),
                        estimated_tokens: text.len() / 4,
                    });
                    context_blocks.push(text);
                }
            }
        }

        // Apply token budgeting
        let mut combined_context = context_blocks.join("\n");
        let budget_chars = max_tokens * 4;
        if combined_context.len() > budget_chars {
            let mut cut = budget_chars;
            while cut > 0 && !combined_context.is_char_boundary(cut) {
                cut -= 1;
            }
            combined_context.truncate(cut);
            combined_context.push_str("\n... [Truncated to stay within token budget]\n");
        }

        // Run deep comprehensive analysis
        let target_dir = targets.iter().find_map(|t| match t {
            TargetKind::LocalDirectory(p) => Some(p.as_path()),
            _ => None,
        }).unwrap_or(workspace_root);

        let analysis_block = match Self::run_comprehensive_analysis(
            Some(target_dir),
            config.probe_network_ports,
            config.probe_timeout_ms,
        ) {
            Ok(report) => Self::format_analysis_markdown(&report),
            Err(e) => format!("⚠️ Comprehensive Analysis error: {e}"),
        };

        let augmented_prompt = format!(
            "{}\n\n--- AUTO-INSPECTED LOCAL & NETWORK FOLDER CONTEXT ---\n{}\n\n--- LIVE SYSTEM & NETWORK ARCHITECTURAL ANALYSIS ---\n{}\n\n[ANALYTICAL DIRECTIVE FOR THE MODEL: The above is real, live inspected infrastructure from the user's computer and network. You MUST directly evaluate and analyze these findings, assess health/hygiene/security, answer the user's prompt using this ground truth, and avoid generic tutorials or hypothetical placeholders.]\n----------------------------------------------------",
            prompt.trim(),
            combined_context.trim(),
            analysis_block.trim()
        );

        Ok((augmented_prompt, reports))
    }

    /// Parse IPv4 address from kernel little-endian hex string representation (e.g. "0101A8C0" -> 192.168.1.1)
    pub fn parse_ipv4_from_hex_le(hex_str: &str) -> Option<Ipv4Addr> {
        if hex_str.len() != 8 {
            return None;
        }
        let val = u32::from_str_radix(hex_str, 16).ok()?;
        let b = val.to_le_bytes();
        Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
    }

    /// Parse /proc/net/route to detect active default network gateways
    pub fn detect_default_gateways() -> Vec<String> {
        let mut gateways = Vec::new();
        let mut seen = HashSet::new();

        if let Ok(file) = File::open("/proc/net/route") {
            let reader = BufReader::new(file);
            for (idx, line) in reader.lines().flatten().enumerate() {
                if idx == 0 {
                    continue; // Skip header: Iface Destination Gateway Flags ...
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let destination = parts[1];
                    let gateway_hex = parts[2];
                    if destination == "00000000" && gateway_hex != "00000000" {
                        if let Some(ip) = Self::parse_ipv4_from_hex_le(gateway_hex) {
                            let ip_str = ip.to_string();
                            if seen.insert(ip_str.clone()) {
                                gateways.push(ip_str);
                            }
                        }
                    }
                }
            }
        }

        if gateways.is_empty() {
            if let Ok(file) = File::open("/etc/resolv.conf") {
                let reader = BufReader::new(file);
                for line in reader.lines().flatten() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("nameserver ") {
                        let ns = trimmed.trim_start_matches("nameserver ").trim();
                        if (ns.starts_with("192.168.") || ns.starts_with("10.") || ns.starts_with("172.16."))
                            && seen.insert(ns.to_string())
                        {
                            gateways.push(ns.to_string());
                        }
                    }
                }
            }
        }

        gateways
    }

    /// Calculate directory disk footprint without following symlinks and bounded by max_entries
    pub fn calculate_directory_size(dir: &Path, max_entries: usize) -> u64 {
        let mut total = 0u64;
        let mut entries_count = 0;
        let mut stack = vec![dir.to_path_buf()];
        let mut visited_inodes = HashSet::new();

        while let Some(current) = stack.pop() {
            if entries_count >= max_entries {
                break;
            }
            #[cfg(unix)]
            {
                if let Ok(meta) = fs::metadata(&current) {
                    if !visited_inodes.insert((meta.dev(), meta.ino())) {
                        continue;
                    }
                }
            }
            if let Ok(rd) = fs::read_dir(&current) {
                for entry in rd.flatten() {
                    entries_count += 1;
                    if entries_count >= max_entries {
                        break;
                    }
                    let path = entry.path();
                    let sym_meta = fs::symlink_metadata(&path);
                    let is_symlink = sym_meta.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
                    if is_symlink {
                        continue;
                    }
                    if let Ok(meta) = fs::metadata(&path) {
                        if meta.is_dir() {
                            stack.push(path);
                        } else {
                            total += meta.len();
                        }
                    }
                }
            }
        }
        total
    }

    /// Deep analysis of a directory structure, archetype, bloat, hygiene score, and security checks
    pub fn analyze_directory_structure(
        dir_path: &Path,
        config: Option<&FolderInspectionConfig>,
    ) -> Result<FolderAnalysisReport> {
        if Self::is_sensitive_target(dir_path) {
            return Err(HgbError::Security(format!(
                "Access to sensitive target blocked: {}",
                dir_path.display()
            )));
        }

        let mut archetype_traits = Vec::new();
        let mut primary_archetype = String::from("Generic Filesystem Directory");

        // 1. Archetype detection
        let cargo_toml = dir_path.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = fs::read_to_string(&cargo_toml).unwrap_or_default();
            if content.contains("[workspace]") {
                primary_archetype = "Rust Cargo Workspace".to_string();
                archetype_traits.push("Rust Cargo Workspace".to_string());
            } else {
                primary_archetype = "Rust Cargo Package".to_string();
                archetype_traits.push("Rust Cargo Crate".to_string());
            }
        }

        let package_json = dir_path.join("package.json");
        if package_json.exists() {
            if dir_path.join("tsconfig.json").exists() {
                archetype_traits.push("TypeScript / Node.js".to_string());
                if primary_archetype.starts_with("Generic") {
                    primary_archetype = "TypeScript / Node.js Project".to_string();
                }
            } else {
                archetype_traits.push("JavaScript / Node.js".to_string());
                if primary_archetype.starts_with("Generic") {
                    primary_archetype = "JavaScript / Node.js Project".to_string();
                }
            }
        }

        if dir_path.join("pyproject.toml").exists()
            || dir_path.join("requirements.txt").exists()
            || dir_path.join("setup.py").exists()
            || dir_path.join("Pipfile").exists()
        {
            archetype_traits.push("Python Ecosystem".to_string());
            if primary_archetype.starts_with("Generic") {
                primary_archetype = "Python Project".to_string();
            }
        }

        if dir_path.join("go.mod").exists() {
            archetype_traits.push("Go Module".to_string());
            if primary_archetype.starts_with("Generic") {
                primary_archetype = "Go Project".to_string();
            }
        }

        if dir_path.join("CMakeLists.txt").exists() || dir_path.join("Makefile").exists() {
            archetype_traits.push("C/C++ Build Environment".to_string());
            if primary_archetype.starts_with("Generic") {
                primary_archetype = "C/C++ Project".to_string();
            }
        }

        if dir_path.join("Dockerfile").exists()
            || dir_path.join("docker-compose.yml").exists()
            || dir_path.join("compose.yaml").exists()
        {
            archetype_traits.push("Docker Containerized".to_string());
        }

        let is_git = dir_path.join(".git").exists();
        if is_git {
            archetype_traits.push("Git Version Controlled".to_string());
        }

        // 2. Build bloat detection
        let mut bloat_items = Vec::new();
        let bloat_candidates = [
            ("target", "target/ (Cargo Build Artifacts)", "Run `cargo clean` to prune debug and release build artifacts"),
            ("node_modules", "node_modules/ (Node.js Dependencies)", "Prune unneeded modules with `npm prune` or delete directory before distribution"),
            (".next", ".next/ (Next.js Application Cache)", "Clean compiler cache with `rm -rf .next` before deploying"),
            ("dist", "dist/ (Bundler Distribution Artifacts)", "Prune compiled output bundles before source packaging"),
            ("build", "build/ (Compiler Output Directory)", "Clean build directory via build system clean targets"),
            ("__pycache__", "__pycache__/ (Python Bytecode Cache)", "Remove Python bytecode cache via `find . -name '__pycache__' -exec rm -rf {} +`"),
            (".pytest_cache", ".pytest_cache/ (Test Suite Cache)", "Clear pytest cache before source packaging"),
            ("venv", "venv/ (Python Virtual Environment)", "Exclude virtualenv from version control and archives"),
            (".venv", ".venv/ (Python Virtual Environment)", "Exclude virtualenv from version control and archives"),
        ];

        for (dirname, label, rec) in &bloat_candidates {
            let candidate_path = dir_path.join(dirname);
            if candidate_path.is_dir() {
                let size = Self::calculate_directory_size(&candidate_path, 8_000);
                if size > 0 {
                    bloat_items.push(BuildBloatItem {
                        path: candidate_path,
                        name: label.to_string(),
                        size_bytes: size,
                        recommendation: rec.to_string(),
                    });
                }
            }
        }

        let total_bloat_bytes: u64 = bloat_items.iter().map(|b| b.size_bytes).sum();

        let default_cfg = FolderInspectionConfig::default();
        let cfg = config.unwrap_or(&default_cfg);

        let mut all_metrics = Vec::new();
        let mut security_warnings = Vec::new();
        let mut total_files = 0usize;
        let mut total_dirs = 0usize;
        let mut total_bytes = 0u64;

        let mut stack = vec![(dir_path.to_path_buf(), 0usize)];
        let mut visited_inodes = HashSet::new();

        while let Some((curr, depth)) = stack.pop() {
            if depth > cfg.max_depth || total_files + total_dirs >= cfg.max_total_entries {
                break;
            }

            #[cfg(unix)]
            {
                if let Ok(meta) = fs::metadata(&curr) {
                    if !visited_inodes.insert((meta.dev(), meta.ino())) {
                        continue;
                    }
                }
            }

            if let Ok(rd) = fs::read_dir(&curr) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    if !cfg.show_hidden && name.starts_with('.') && name != ".git" {
                        continue;
                    }
                    if Self::is_sensitive_target(&p) {
                        continue;
                    }

                    let sym_meta = fs::symlink_metadata(&p);
                    let is_symlink = sym_meta.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
                    let meta = fs::metadata(&p).ok().or_else(|| sym_meta.ok());

                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

                    #[cfg(unix)]
                    let mode = meta.as_ref().map(|m| m.mode()).unwrap_or(0);
                    #[cfg(not(unix))]
                    let mode = 0u32;

                    #[cfg(unix)]
                    {
                        if mode & 0o002 != 0 {
                            security_warnings.push(SecurityWarning {
                                path: p.clone(),
                                issue: format!("World-writable permissions (mode: {:o})", mode & 0o777),
                                severity: "High".to_string(),
                                recommendation: format!("Restrict world write permissions: `chmod o-w \"{}\"`", p.display()),
                            });
                        }
                        if mode & 0o6000 != 0 {
                            security_warnings.push(SecurityWarning {
                                path: p.clone(),
                                issue: format!("Elevated setuid/setgid bits detected (mode: {:o})", mode),
                                severity: "High".to_string(),
                                recommendation: format!("Verify if setuid/setgid is strictly required: `chmod u-s,g-s \"{}\"`", p.display()),
                            });
                        }
                        let lower_name = name.to_lowercase();
                        if lower_name.starts_with(".env")
                            || lower_name.ends_with(".key")
                            || lower_name.ends_with(".pem")
                            || lower_name.contains("id_rsa")
                            || lower_name.contains("id_ed25519")
                        {
                            if mode & 0o077 != 0 {
                                security_warnings.push(SecurityWarning {
                                    path: p.clone(),
                                    issue: format!("Sensitive credential/key file has permissive group/other access (mode: {:o})", mode & 0o777),
                                    severity: "Critical".to_string(),
                                    recommendation: format!("Tighten secret file permissions immediately: `chmod 600 \"{}\"`", p.display()),
                                });
                            }
                        }
                    }

                    if is_dir {
                        total_dirs += 1;
                        all_metrics.push(FileEntryMetric {
                            path: p.clone(),
                            name: format!("{name}/"),
                            size_bytes: size,
                            is_dir: true,
                        });
                        if name != "target" && name != "node_modules" && name != ".git" && !is_symlink {
                            stack.push((p, depth + 1));
                        }
                    } else {
                        total_files += 1;
                        total_bytes += size;
                        all_metrics.push(FileEntryMetric {
                            path: p,
                            name,
                            size_bytes: size,
                            is_dir: false,
                        });
                    }
                }
            }
        }

        all_metrics.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        let largest_items = all_metrics.into_iter().take(10).collect();

        let mut hygiene_score = 100i32;
        let mut hygiene_findings = Vec::new();

        hygiene_findings.push(format!("Primary Archetype: {primary_archetype}"));
        if !archetype_traits.is_empty() {
            hygiene_findings.push(format!("Detected Archetype Traits: {}", archetype_traits.join(", ")));
        }

        if total_bloat_bytes >= 5 * 1024 * 1024 * 1024 {
            hygiene_score -= 30;
            hygiene_findings.push(format!("Severe build artifact bloat: {} in cache directories", Self::format_bytes(total_bloat_bytes)));
        } else if total_bloat_bytes >= 1024 * 1024 * 1024 {
            hygiene_score -= 20;
            hygiene_findings.push(format!("Substantial build artifact bloat: {} in cache directories", Self::format_bytes(total_bloat_bytes)));
        } else if total_bloat_bytes >= 200 * 1024 * 1024 {
            hygiene_score -= 10;
            hygiene_findings.push(format!("Moderate build artifact footprint: {} in cache directories", Self::format_bytes(total_bloat_bytes)));
        } else {
            hygiene_findings.push("Build artifact bloat: Minimal / Pristine".to_string());
        }

        let mut crit_count = 0;
        let mut high_count = 0;
        for w in &security_warnings {
            if w.severity == "Critical" {
                crit_count += 1;
                hygiene_score -= 25;
            } else if w.severity == "High" {
                high_count += 1;
                hygiene_score -= 15;
            } else {
                hygiene_score -= 5;
            }
        }

        if crit_count > 0 || high_count > 0 {
            hygiene_findings.push(format!("Security posture alert: {crit_count} Critical, {high_count} High permission warnings detected"));
        } else {
            hygiene_findings.push("Security posture: Clean (no world-writable or exposed secret files found)".to_string());
        }

        if is_git && !dir_path.join(".gitignore").exists() {
            hygiene_score -= 10;
            hygiene_findings.push("Notice: Git repository lacks a `.gitignore` file".to_string());
        }

        let final_score = hygiene_score.clamp(0, 100) as u32;

        Ok(FolderAnalysisReport {
            path: dir_path.to_path_buf(),
            archetype: primary_archetype,
            archetype_traits,
            total_files,
            total_dirs,
            total_bytes,
            largest_items,
            bloat_items,
            total_bloat_bytes,
            hygiene_score: final_score,
            hygiene_findings,
            security_warnings,
        })
    }

    /// Deep storage drive saturation, reliability, and security analysis
    pub fn analyze_storage_drives() -> StorageAnalysisReport {
        let raw_drives = Self::discover_storage_drives();
        let mut analyzed_drives = Vec::new();
        let mut critical_partitions = Vec::new();
        let mut system_warnings = Vec::new();
        let mut recommendations = Vec::new();

        let mut total_cap = 0u64;
        let mut total_avail = 0u64;

        for d in raw_drives {
            let used_bytes = match (d.total_bytes, d.available_bytes) {
                (Some(tot), Some(avail)) => {
                    total_cap += tot;
                    total_avail += avail;
                    Some(tot.saturating_sub(avail))
                }
                _ => None,
            };

            let usage_percent = match (d.total_bytes, used_bytes) {
                (Some(tot), Some(used)) if tot > 0 => Some((used as f64 / tot as f64) * 100.0),
                _ => None,
            };

            let free_percent = usage_percent.map(|u| 100.0 - u);

            let is_root = d.mount_point == Path::new("/");
            let low_free_bytes = d.available_bytes.map(|a| a < 5 * 1024 * 1024 * 1024).unwrap_or(false);
            let is_critical = usage_percent.map(|u| u >= 85.0).unwrap_or(false)
                || free_percent.map(|f| f < 15.0).unwrap_or(false)
                || (is_root && low_free_bytes);

            let warning = if is_critical {
                let avail_str = d.available_bytes.map(Self::format_bytes).unwrap_or_else(|| "unknown".to_string());
                let pct_str = usage_percent.map(|u| format!("{:.1}%", u)).unwrap_or_default();
                let msg = format!("Partition {} is critically saturated ({} utilized, only {} free space remaining)", d.mount_point.display(), pct_str, avail_str);
                critical_partitions.push(d.mount_point.display().to_string());
                Some(msg)
            } else {
                None
            };

            let reliability = match d.fstype.as_str() {
                "ext4" | "xfs" | "btrfs" | "zfs" => "High (Journaled / Copy-on-Write Enterprise)".to_string(),
                "f2fs" => "High (Flash-Friendly Filesystem)".to_string(),
                "vfat" | "fat" | "exfat" => "Basic (FAT non-POSIX, no UNIX file ownership guarantees)".to_string(),
                "ntfs" => "Compatibility (NTFS via FUSE)".to_string(),
                _ => "Standard POSIX".to_string(),
            };

            let mount_flags: Vec<String> = d.options.split(',').map(|s| s.trim().to_string()).collect();
            let mut security_audit = Vec::new();

            if mount_flags.iter().any(|f| f == "ro") {
                security_audit.push("Mounted Read-Only (ro)".to_string());
            } else if mount_flags.iter().any(|f| f == "rw") {
                security_audit.push("Mounted Read-Write (rw)".to_string());
            }

            if !is_root && d.mount_point != Path::new("/usr") {
                if !mount_flags.iter().any(|f| f == "nosuid") {
                    security_audit.push("Notice: Missing 'nosuid' option on non-root volume".to_string());
                }
                if !mount_flags.iter().any(|f| f == "nodev") {
                    security_audit.push("Notice: Missing 'nodev' option on non-root volume".to_string());
                }
            }

            if mount_flags.iter().any(|f| f == "noexec") {
                security_audit.push("Security hardened: Execution disabled ('noexec')".to_string());
            }

            analyzed_drives.push(StorageDriveAnalysis {
                device: d.device,
                mount_point: d.mount_point,
                fstype: d.fstype,
                options: d.options,
                total_bytes: d.total_bytes,
                available_bytes: d.available_bytes,
                used_bytes,
                usage_percent,
                is_critical,
                warning,
                filesystem_reliability: reliability,
                mount_flags,
                security_audit,
            });
        }

        let overall_usage_percent = if total_cap > 0 {
            let used = total_cap.saturating_sub(total_avail);
            (used as f64 / total_cap as f64) * 100.0
        } else {
            0.0
        };

        if !critical_partitions.is_empty() {
            system_warnings.push(format!(
                "CRITICAL: {} partition(s) exceed 85% capacity threshold: {}",
                critical_partitions.len(),
                critical_partitions.join(", ")
            ));
            recommendations.push(
                "Immediate storage remediation: Clean package caches (`apt clean` / `dnf clean all`), prune old Docker images (`docker system prune`), or truncate application logs under `/var/log`."
                    .to_string(),
            );
        } else {
            recommendations.push(
                "Storage headroom is nominal across all mounted drives (> 15% available free space)."
                    .to_string(),
            );
        }

        StorageAnalysisReport {
            drives: analyzed_drives,
            total_capacity_bytes: total_cap,
            total_available_bytes: total_avail,
            overall_usage_percent,
            critical_partitions,
            system_warnings,
            recommendations,
        }
    }

    /// Deep network infrastructure, gateway detection, and active host classification analysis
    pub fn analyze_network_infrastructure(probe_ports: bool, timeout_ms: u64) -> NetworkAnalysisReport {
        let detected_gateways = Self::detect_default_gateways();
        let raw_computers = Self::discover_network_computers(probe_ports, timeout_ms);
        let raw_shares = Self::discover_network_shares();

        let mut active_hosts = Vec::new();
        let mut classification_summary: HashMap<String, usize> = HashMap::new();
        let mut reachability = Vec::new();
        let mut security_considerations = Vec::new();

        for comp in raw_computers {
            let is_gateway = detected_gateways.contains(&comp.ip_address) || comp.ip_address.ends_with(".1") || comp.ip_address.ends_with(".254");
            let is_multicast = comp.ip_address.starts_with("224.")
                || comp.ip_address.starts_with("239.")
                || comp.ip_address == "255.255.255.255"
                || comp.ip_address.starts_with("ff0");

            let category = if is_gateway {
                "Gateway/Router".to_string()
            } else if comp.smb_open == Some(true) || comp.nfs_open == Some(true) {
                "NAS/File Server".to_string()
            } else if is_multicast {
                "IPv6 Multicast/Broadcast".to_string()
            } else {
                "Workstations".to_string()
            };

            let mut notes = Vec::new();
            if is_gateway {
                notes.push("Default Subnet Gateway & Internet Uplink".to_string());
            }
            if comp.smb_open == Some(true) {
                notes.push("Open SMB (port 445) File Sharing Active".to_string());
                security_considerations.push(format!(
                    "Notice: Host {} has port 445 (SMB) exposed on local network",
                    comp.ip_address
                ));
            }
            if comp.nfs_open == Some(true) {
                notes.push("Open NFS (port 2049) Network Export Active".to_string());
            }
            if comp.hostname.is_some() {
                notes.push(format!("Resolved Hostname: {}", comp.hostname.as_ref().unwrap()));
            }

            *classification_summary.entry(category.clone()).or_insert(0) += 1;

            active_hosts.push(NetworkHostAnalysis {
                ip_address: comp.ip_address,
                hostname: comp.hostname,
                mac_address: comp.mac_address,
                interface: comp.interface,
                category,
                is_gateway,
                smb_open: comp.smb_open,
                nfs_open: comp.nfs_open,
                notes,
            });
        }

        for share in &raw_shares {
            let opts = &share.options;
            if opts.contains("vers=1.0") {
                security_considerations.push(format!(
                    "CRITICAL: Share `{}` uses deprecated and vulnerable SMBv1 (vers=1.0) protocol!",
                    share.device_or_remote
                ));
            }
            if opts.contains("guest") || opts.contains("sec=none") {
                security_considerations.push(format!(
                    "WARNING: Share `{}` is mounted with unauthenticated guest access",
                    share.device_or_remote
                ));
            }
        }

        if !detected_gateways.is_empty() {
            reachability.push(format!(
                "Default Gateway Router(s) identified: {}. Routing uplink active.",
                detected_gateways.join(", ")
            ));
        } else {
            reachability.push("Default Gateway Router: Not explicitly identified in routing table".to_string());
        }

        reachability.push(format!(
            "LAN Active Computer Inventory: {} live hosts discovered via kernel ARP cache.",
            active_hosts.len()
        ));

        let nas_count = classification_summary.get("NAS/File Server").copied().unwrap_or(0);
        if nas_count > 0 {
            reachability.push(format!("{nas_count} NAS/File Server(s) detected with active SMB/NFS storage ports."));
        }

        NetworkAnalysisReport {
            detected_gateways,
            active_hosts,
            host_classification_summary: classification_summary,
            mounted_shares: raw_shares,
            reachability_assessment: reachability,
            security_considerations,
        }
    }

    /// Comprehensive system, storage, and network analysis synthesizing an executive diagnosis
    pub fn run_comprehensive_analysis(
        dir_path: Option<&Path>,
        probe_network_ports: bool,
        timeout_ms: u64,
    ) -> Result<ComprehensiveSystemAnalysisReport> {
        let folder_rep = match dir_path {
            Some(p) => Some(Self::analyze_directory_structure(p, None)?),
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                Self::analyze_directory_structure(&cwd, None).ok()
            }
        };

        let storage_rep = Self::analyze_storage_drives();
        let network_rep = Self::analyze_network_infrastructure(probe_network_ports, timeout_ms);

        let mut diagnosis_parts = Vec::new();

        if !storage_rep.critical_partitions.is_empty() {
            diagnosis_parts.push(format!(
                "CRITICAL STORAGE DEFICIT: Partitions {} exceed 85% saturation threshold.",
                storage_rep.critical_partitions.join(", ")
            ));
        } else {
            diagnosis_parts.push(format!(
                "Storage saturation is nominal ({:.1}% capacity utilized across {} volumes).",
                storage_rep.overall_usage_percent,
                storage_rep.drives.len()
            ));
        }

        if let Some(ref f) = folder_rep {
            diagnosis_parts.push(format!(
                "Workspace archetype identified as {} (Hygiene Score: {}/100).",
                f.archetype, f.hygiene_score
            ));
            if f.total_bloat_bytes > 500 * 1024 * 1024 {
                diagnosis_parts.push(format!(
                    "Cache bloat of {} detected across build directories.",
                    Self::format_bytes(f.total_bloat_bytes)
                ));
            }
        }

        let gw_str = if !network_rep.detected_gateways.is_empty() {
            format!("Default Gateway ({}) active", network_rep.detected_gateways.join(", "))
        } else {
            "No default gateway detected".to_string()
        };
        diagnosis_parts.push(format!(
            "Local subnet topology: {}, {} active host(s) discovered, {} mounted network share(s).",
            gw_str,
            network_rep.active_hosts.len(),
            network_rep.mounted_shares.len()
        ));

        let executive_diagnosis = diagnosis_parts.join(" ");

        let has_crit_storage = !storage_rep.critical_partitions.is_empty();
        let has_crit_security = folder_rep.as_ref().map(|f| f.security_warnings.iter().any(|w| w.severity == "Critical")).unwrap_or(false);
        let has_crit_network = network_rep.security_considerations.iter().any(|s| s.starts_with("CRITICAL"));

        let has_warning_storage = storage_rep.drives.iter().any(|d| d.usage_percent.map(|u| u >= 80.0).unwrap_or(false));
        let has_warning_folder = folder_rep.as_ref().map(|f| f.hygiene_score < 80 || f.total_bloat_bytes > 1024 * 1024 * 1024).unwrap_or(false);

        let health_verdict = if has_crit_storage || has_crit_security || has_crit_network {
            "CRITICAL ATTENTION REQUIRED".to_string()
        } else if has_warning_storage || has_warning_folder {
            "WARNING / SUB-OPTIMAL".to_string()
        } else {
            "HEALTHY & NOMINAL".to_string()
        };

        let mut actionable_recommendations = Vec::new();
        actionable_recommendations.extend(storage_rep.recommendations.clone());
        if let Some(ref f) = folder_rep {
            for b in &f.bloat_items {
                actionable_recommendations.push(format!("{}: {}", b.name, b.recommendation));
            }
            for w in &f.security_warnings {
                actionable_recommendations.push(format!("[{}] {}", w.severity, w.recommendation));
            }
        }
        for sec in &network_rep.security_considerations {
            actionable_recommendations.push(sec.clone());
        }

        Ok(ComprehensiveSystemAnalysisReport {
            folder_analysis: folder_rep,
            storage_analysis: Some(storage_rep),
            network_analysis: Some(network_rep),
            executive_diagnosis,
            health_verdict,
            actionable_recommendations,
        })
    }

    /// Format comprehensive architectural analysis into high-signal Markdown
    pub fn format_analysis_markdown(report: &ComprehensiveSystemAnalysisReport) -> String {
        let mut out = String::new();
        out.push_str("# 🏢 Comprehensive System & Network Architectural Report\n");
        out.push_str(&format!("**System Health Verdict:** `[ {} ]`\n", report.health_verdict));
        out.push_str(&format!("**Executive Diagnosis:** {}\n\n", report.executive_diagnosis));

        if let Some(ref st) = report.storage_analysis {
            out.push_str("### 💾 Storage Capacity & Partition Saturation Assessment\n");
            out.push_str(&format!(
                "- **Overall Storage Utilized:** {:.1}% ({} available / {} total capacity)\n",
                st.overall_usage_percent,
                Self::format_bytes(st.total_available_bytes),
                Self::format_bytes(st.total_capacity_bytes)
            ));
            if !st.critical_partitions.is_empty() {
                out.push_str(&format!("- ⚠️ **Critical Partitions (>85% full):** {}\n", st.critical_partitions.join(", ")));
            }
            out.push_str("\n| Device | Mount Point | Filesystem | Used / Total | Usage % | Reliability | Flags |\n");
            out.push_str("| :--- | :--- | :--- | :--- | :--- | :--- | :--- |\n");
            for d in &st.drives {
                let usage_str = d.usage_percent.map(|u| format!("{:.1}%", u)).unwrap_or_else(|| "N/A".to_string());
                let space_str = match (d.used_bytes, d.total_bytes) {
                    (Some(u), Some(t)) => format!("{} / {}", Self::format_bytes(u), Self::format_bytes(t)),
                    _ => "N/A".to_string(),
                };
                let status_icon = if d.is_critical { "🔴 CRITICAL" } else { "🟢 Nominal" };
                let short_flags = if d.mount_flags.len() > 3 {
                    format!("{}, ...", d.mount_flags[..2].join(","))
                } else {
                    d.mount_flags.join(",")
                };
                out.push_str(&format!(
                    "| `{}` | `{}` | `{}` | {} | {} [{status_icon}] | {} | `{}` |\n",
                    d.device, d.mount_point.display(), d.fstype, space_str, usage_str, d.filesystem_reliability, short_flags
                ));
            }
            out.push('\n');
            for w in &st.system_warnings {
                out.push_str(&format!("- ⚠️ {w}\n"));
            }
            out.push('\n');
        }

        if let Some(ref net) = report.network_analysis {
            out.push_str("### 🌐 Network Subnet Topology & Host Classification\n");
            if !net.detected_gateways.is_empty() {
                out.push_str(&format!("- **Identified Gateway Router(s):** `{}`\n", net.detected_gateways.join("`, `")));
            }
            let summary_str: Vec<String> = net.host_classification_summary.iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            if !summary_str.is_empty() {
                out.push_str(&format!("- **Host Inventory:** {}\n", summary_str.join(" | ")));
            }

            if !net.active_hosts.is_empty() {
                out.push_str("\n| IP Address | Hostname | MAC Address | Category | SMB (445) | NFS (2049) | Notes |\n");
                out.push_str("| :--- | :--- | :--- | :--- | :--- | :--- | :--- |\n");
                for h in &net.active_hosts {
                    let host_name = h.hostname.as_deref().unwrap_or("-");
                    let mac_str = h.mac_address.as_deref().unwrap_or("-");
                    let smb_str = match h.smb_open {
                        Some(true) => "🟢 Open",
                        Some(false) => "⚪ Closed",
                        None => "❓ Unprobed",
                    };
                    let nfs_str = match h.nfs_open {
                        Some(true) => "🟢 Open",
                        Some(false) => "⚪ Closed",
                        None => "❓ Unprobed",
                    };
                    let notes_str = if h.notes.is_empty() { "-".to_string() } else { h.notes.join("; ") };
                    out.push_str(&format!(
                        "| `{}` | {} | `{}` | **{}** | {} | {} | {} |\n",
                        h.ip_address, host_name, mac_str, h.category, smb_str, nfs_str, notes_str
                    ));
                }
                out.push('\n');
            }

            if !net.mounted_shares.is_empty() {
                out.push_str("#### Mounted Network Shares:\n");
                for s in &net.mounted_shares {
                    out.push_str(&format!("- Remote: `{}` mounted on `{}` [{}]\n", s.device_or_remote, s.mount_point.display(), s.fstype));
                }
                out.push('\n');
            }

            for sec in &net.security_considerations {
                out.push_str(&format!("- 🛡️ {sec}\n"));
            }
            out.push('\n');
        }

        if let Some(ref f) = report.folder_analysis {
            out.push_str("### 📁 Filesystem Architecture & Workspace Profile\n");
            out.push_str(&format!("- **Target Path:** `{}`\n", f.path.display()));
            out.push_str(&format!("- **Project Archetype:** **{}**\n", f.archetype));
            if !f.archetype_traits.is_empty() {
                out.push_str(&format!("- **Archetype Traits:** {}\n", f.archetype_traits.join(", ")));
            }
            out.push_str(&format!("- **Hygiene Score:** `{}/100`\n", f.hygiene_score));
            out.push_str(&format!(
                "- **Total Entries:** {} files, {} directories ({})\n",
                f.total_files, f.total_dirs, Self::format_bytes(f.total_bytes)
            ));

            if !f.bloat_items.is_empty() {
                out.push_str(&format!("\n#### Build Bloat & Cache Artifacts (Total: {}):\n", Self::format_bytes(f.total_bloat_bytes)));
                for b in &f.bloat_items {
                    out.push_str(&format!("- **{}** ({}): {}\n", b.name, Self::format_bytes(b.size_bytes), b.recommendation));
                }
            }

            if !f.security_warnings.is_empty() {
                out.push_str("\n#### Security & Permission Warnings:\n");
                for w in &f.security_warnings {
                    out.push_str(&format!("- ⚠️ **[{}]** `{}`: {} => {}\n", w.severity, w.path.display(), w.issue, w.recommendation));
                }
            }

            if !f.largest_items.is_empty() {
                out.push_str("\n#### Largest File/Directory Entries:\n");
                for item in &f.largest_items {
                    let kind_indicator = if item.is_dir { "📁" } else { "📄" };
                    out.push_str(&format!("- {} `{}` ({})\n", kind_indicator, item.path.display(), Self::format_bytes(item.size_bytes)));
                }
            }
            out.push('\n');
        }

        if !report.actionable_recommendations.is_empty() {
            out.push_str("### 🛠️ Actionable Architectural Recommendations\n");
            for rec in &report.actionable_recommendations {
                out.push_str(&format!("- [ ] {}\n", rec));
            }
            out.push('\n');
        }

        out
    }

    /// Convenience wrapper using current directory as workspace root
    pub fn augment_prompt_with_folders(
        prompt: &str,
        max_tokens: usize,
    ) -> Result<(String, Vec<InspectedTargetReport>)> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::augment_prompt_with_folders_ctx(&cwd, prompt, max_tokens)
    }

    fn format_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;
        if bytes >= GB {
            format!("{:.1} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.1} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.1} KB", bytes as f64 / KB as f64)
        } else {
            format!("{bytes} B")
        }
    }

    fn format_mode(mode: u32) -> String {
        #[cfg(unix)]
        {
            let u = format!(
                "{}{}{}",
                if mode & 0o400 != 0 { 'r' } else { '-' },
                if mode & 0o200 != 0 { 'w' } else { '-' },
                if mode & 0o100 != 0 { 'x' } else { '-' }
            );
            let g = format!(
                "{}{}{}",
                if mode & 0o040 != 0 { 'r' } else { '-' },
                if mode & 0o020 != 0 { 'w' } else { '-' },
                if mode & 0o010 != 0 { 'x' } else { '-' }
            );
            let o = format!(
                "{}{}{}",
                if mode & 0o004 != 0 { 'r' } else { '-' },
                if mode & 0o002 != 0 { 'w' } else { '-' },
                if mode & 0o001 != 0 { 'x' } else { '-' }
            );
            format!("{u}{g}{o}")
        }
        #[cfg(not(unix))]
        {
            "rw-r--r--".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn test_path_detection_explicit_and_implicit() {
        let temp_dir = std::env::temp_dir().join("hgb_test_detection");
        let _ = fs::create_dir_all(&temp_dir);

        let prompt = format!(
            "Please check {} and scan network shares along with smb://192.168.1.100/data and ~/Documents",
            temp_dir.display()
        );

        let targets = FolderInspectorEngine::detect_targets(Path::new("."), &prompt);
        assert!(!targets.is_empty());

        let has_temp = targets.iter().any(|t| match t {
            TargetKind::LocalDirectory(p) => p == &temp_dir,
            _ => false,
        });
        assert!(has_temp, "Expected temp_dir to be detected");

        let has_smb = targets.iter().any(|t| match t {
            TargetKind::NetworkUri(u) => u.starts_with("smb://"),
            _ => false,
        });
        assert!(has_smb, "Expected smb uri to be detected");

        let has_net_discovery = targets.iter().any(|t| matches!(t, TargetKind::NetworkDiscovery));
        assert!(has_net_discovery, "Expected network discovery intent to be detected");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_real_directory_inspection_tree_and_types() {
        let base = std::env::temp_dir().join("hgb_test_tree");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src")).unwrap();

        // Write a Rust file
        let mut rs_file = File::create(base.join("src/lib.rs")).unwrap();
        writeln!(rs_file, "pub fn hello() -> &'static str {{ \"world\" }}").unwrap();

        // Write a binary file
        let mut bin_file = File::create(base.join("binary.bin")).unwrap();
        bin_file.write_all(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00").unwrap();

        let config = FolderInspectionConfig::default();
        let (tree, summary) = FolderInspectorEngine::inspect_directory(&base, &config).unwrap();

        assert!(summary.total_files >= 2);
        assert!(summary.total_dirs >= 1);
        assert!(tree.contains("lib.rs"));
        assert!(tree.contains("[Rust]"));
        assert!(tree.contains("binary.bin"));
        assert!(tree.contains("[Binary]"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_symlink_cycle_protection() {
        let base = std::env::temp_dir().join("hgb_test_symlink_cycle");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub")).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link_path = base.join("sub/cyclic_link");
            let _ = symlink(&base, link_path);

            let config = FolderInspectionConfig {
                max_depth: 5,
                ..Default::default()
            };
            let (tree, summary) = FolderInspectorEngine::inspect_directory(&base, &config).unwrap();
            assert!(tree.contains("CYCLIC SYMLINK") || summary.total_dirs <= 4);
        }

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_sensitive_file_guardrail() {
        let shadow_path = Path::new("/etc/shadow");
        assert!(FolderInspectorEngine::is_sensitive_target(shadow_path));

        let res = FolderInspectorEngine::inspect_directory(
            shadow_path,
            &FolderInspectionConfig::default(),
        );
        assert!(res.is_err(), "Must block sensitive file inspection");
    }

    #[test]
    fn test_real_linux_storage_and_network_discovery() {
        let drives = FolderInspectorEngine::discover_storage_drives();
        // On any genuine Linux environment with /proc/mounts, at least root / is mounted
        assert!(!drives.is_empty(), "Should discover mounted storage drives");

        let shares = FolderInspectorEngine::discover_network_shares();
        println!("Discovered network shares: {:?}", shares);

        let comps = FolderInspectorEngine::discover_network_computers(false, 100);
        println!("Discovered network computers: {:?}", comps);
    }

    #[test]
    fn test_prompt_augmentation_injection() {
        let prompt = "How do I organize files in ./crates/hgb-core and list storage drives?";
        let (augmented, reports) =
            FolderInspectorEngine::augment_prompt_with_folders(prompt, 1000).unwrap();

        assert!(!reports.is_empty());
        assert!(augmented.contains("--- AUTO-INSPECTED LOCAL & NETWORK FOLDER CONTEXT ---"));
        assert!(augmented.contains("--- LIVE SYSTEM & NETWORK ARCHITECTURAL ANALYSIS ---"));
        assert!(augmented.contains("[ANALYTICAL DIRECTIVE FOR THE MODEL:"));
        assert!(augmented.contains("----------------------------------------------------"));
    }

    #[test]
    fn test_deep_directory_analysis_and_bloat() {
        let base = std::env::temp_dir().join("hgb_test_deep_analysis");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("target/debug")).unwrap();
        fs::create_dir_all(base.join("src")).unwrap();

        let mut cargo_file = File::create(base.join("Cargo.toml")).unwrap();
        writeln!(cargo_file, "[package]\nname = \"test_crate\"\nversion = \"0.1.0\"\n").unwrap();

        let mut dummy_bin = File::create(base.join("target/debug/dummy")).unwrap();
        dummy_bin.write_all(&vec![0u8; 1024 * 50]).unwrap(); // 50 KB dummy artifact

        let report = FolderInspectorEngine::analyze_directory_structure(&base, None).unwrap();
        assert_eq!(report.archetype, "Rust Cargo Package");
        assert!(!report.bloat_items.is_empty());
        assert!(report.total_bloat_bytes >= 1024 * 50);
        assert!(report.hygiene_score > 0);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_deep_storage_and_network_analysis() {
        let storage_rep = FolderInspectorEngine::analyze_storage_drives();
        assert!(!storage_rep.drives.is_empty());
        assert!(storage_rep.total_capacity_bytes > 0);

        let net_rep = FolderInspectorEngine::analyze_network_infrastructure(false, 100);
        assert!(!net_rep.reachability_assessment.is_empty());

        let comp_rep = FolderInspectorEngine::run_comprehensive_analysis(None, false, 100).unwrap();
        assert!(!comp_rep.executive_diagnosis.is_empty());
        assert!(!comp_rep.health_verdict.is_empty());

        let md = FolderInspectorEngine::format_analysis_markdown(&comp_rep);
        assert!(md.contains("# 🏢 Comprehensive System & Network Architectural Report"));
        assert!(md.contains("System Health Verdict"));
    }

    #[test]
    fn test_detect_folder_with_spaces() {
        let temp_base = std::env::temp_dir().join("hgb_spaces_test");
        let spaced_folder = temp_base.join("My AI Project");
        let _ = fs::create_dir_all(&spaced_folder);

        // 1. Quoted folder name
        let prompt_quoted = format!("inspect \"{}\" and storage", spaced_folder.display());
        let targets1 = FolderInspectorEngine::detect_targets(&temp_base, &prompt_quoted);
        assert!(
            targets1.iter().any(|t| match t {
                TargetKind::LocalDirectory(p) => p == &spaced_folder,
                _ => false,
            }),
            "Expected quoted folder with spaces to be detected"
        );

        // 2. Unquoted path with spaces
        let prompt_unquoted = format!("check path {} now", spaced_folder.display());
        let targets2 = FolderInspectorEngine::detect_targets(&temp_base, &prompt_unquoted);
        assert!(
            targets2.iter().any(|t| match t {
                TargetKind::LocalDirectory(p) => p == &spaced_folder,
                _ => false,
            }),
            "Expected unquoted path with spaces to be detected"
        );

        // 3. Trigger word with relative folder name with spaces
        let prompt_trigger = "please inspect folder My AI Project and check health";
        let targets3 = FolderInspectorEngine::detect_targets(&temp_base, prompt_trigger);
        assert!(
            targets3.iter().any(|t| match t {
                TargetKind::LocalDirectory(p) => p == &spaced_folder,
                _ => false,
            }),
            "Expected folder with spaces via trigger to be detected"
        );

        // 4. Subdirectory scan with spaces
        let prompt_sub = "what is going on in My AI Project today?";
        let targets4 = FolderInspectorEngine::detect_targets(&temp_base, prompt_sub);
        assert!(
            targets4.iter().any(|t| match t {
                TargetKind::LocalDirectory(p) => p == &spaced_folder,
                _ => false,
            }),
            "Expected directory with spaces via direct name to be detected"
        );

        let _ = fs::remove_dir_all(&temp_base);
    }
}

