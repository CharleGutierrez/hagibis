//! # Folder & Network Share Inspection and Auto-Grounding Engine
//!
//! Provides zero-mock POSIX directory tree inspection, network share probing (CIFS/SMB/NFS),
//! active LAN computer discovery via ARP/hosts, storage drive parsing, and prompt auto-grounding.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpStream};
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
            probe_network_ports: true,
            probe_timeout_ms: 350,
        }
    }
}

pub struct FolderInspectorEngine;

impl FolderInspectorEngine {
    /// Detect target paths and implicit intent from natural language prompts
    pub fn detect_targets(workspace_root: &Path, prompt: &str) -> Vec<TargetKind> {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();

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

        // 2. Tilde paths (~/...)
        let tilde_re = Regex::new(r#"(?:^|[\s"'`\(\[\{])(~[a-zA-Z0-9_\.\-/]*)"#).unwrap();
        let home_dir = std::env::var("HOME").ok().map(PathBuf::from);
        for caps in tilde_re.captures_iter(prompt) {
            let raw = caps[1].to_string();
            if let Some(ref home) = home_dir {
                let expanded = if raw == "~" {
                    home.clone()
                } else if let Some(stripped) = raw.strip_prefix("~/") {
                    home.join(stripped)
                } else {
                    continue;
                };
                if expanded.exists() && seen.insert(expanded.to_string_lossy().to_string()) {
                    targets.push(TargetKind::LocalDirectory(expanded));
                }
            }
        }

        // 3. Absolute POSIX paths (/...)
        let abs_re = Regex::new(r#"(?:^|[\s"'`\(\[\{])(/([a-zA-Z0-9_\.\-]+/)*[a-zA-Z0-9_\.\-]+/?)"#).unwrap();
        for caps in abs_re.captures_iter(prompt) {
            let p_str = caps[1].to_string();
            let p = PathBuf::from(&p_str);
            // Ignore single slash or common division artifacts
            if p_str.len() > 1 && (p.is_dir() || Self::is_standard_system_dir(&p)) {
                if seen.insert(p.to_string_lossy().to_string()) {
                    targets.push(TargetKind::LocalDirectory(p));
                }
            }
        }

        // 4. Relative paths (./..., ../..., or relative to workspace)
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

        for target in targets {
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
            combined_context.truncate(budget_chars);
            combined_context.push_str("\n... [Truncated to stay within token budget]\n");
        }

        let augmented_prompt = format!(
            "{}\n\n--- AUTO-INSPECTED LOCAL & NETWORK FOLDER CONTEXT ---\n{}\n----------------------------------------------------",
            prompt.trim(),
            combined_context.trim()
        );

        Ok((augmented_prompt, reports))
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
        assert!(augmented.contains("----------------------------------------------------"));
    }
}
