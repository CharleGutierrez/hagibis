//! # Universal Prompt Auto-Grounding & Analytical Context Engine
//!
//! Provides zero-mock, real-time multimodal prompt auto-grounding across:
//! 1. External Web URLs (HTTP/HTTPS, markdown links, scheme-less popular domains)
//! 2. Computer folders, local file trees, storage partition saturation, and LAN computers
//! 3. Specific local source & config files on disk
//! 4. Real-time host system specifications (OS, kernel, CPU model, RAM, load average)
//!
//! Grounded context is formatted into high-signal Markdown envelopes with authoritative directives,
//! ensuring Local LLMs (Ollama) and Frontier LLMs (Gemini, Claude, GPT-4o, Codex) analyze the live
//! ground truth without excuses or claiming lack of access.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};
use crate::folder_inspector::{
    FolderInspectionConfig, FolderInspectorEngine, InspectedTargetReport, TargetKind,
};
use crate::web_browser::{WebBrowserEngine, WebPageContent};

/// Safe preview of a local source or configuration file detected in the prompt
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectedFileTarget {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub preview_content: String,
    pub token_estimate: usize,
    pub line_count: usize,
}

/// Real-time summary of host system hardware, kernel, and resource saturation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSpecsSummary {
    pub os: String,
    pub kernel: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub load_avg: String,
}

/// Comprehensive report of all grounded targets collected for a prompt
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GroundingContextReport {
    pub web_sources: Vec<WebPageContent>,
    pub inspected_targets: Vec<InspectedTargetReport>,
    pub file_previews: Vec<InspectedFileTarget>,
    pub system_spec: Option<SystemSpecsSummary>,
    pub estimated_total_tokens: usize,
}

impl GroundingContextReport {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.web_sources.is_empty()
            && self.inspected_targets.is_empty()
            && self.file_previews.is_empty()
            && self.system_spec.is_none()
    }
}

/// Universal Prompt Auto-Grounder for Hagibis
pub struct UniversalPromptGrounder;

impl UniversalPromptGrounder {
    /// Authoritative directive appended to every grounded context envelope
    pub const AUTHORITATIVE_DIRECTIVE: &'static str =
        "[AUTHORITATIVE DIRECTIVE FOR THE AI MODEL:\n\
The above context contains real, live ground-truth data automatically inspected from the user's computer, filesystem, network infrastructure, hardware, and external web links in real-time.\n\
You MUST directly evaluate, inspect, and synthesize these findings to answer the user's prompt.\n\
NEVER state or claim that you cannot browse the internet, access external links, view local files, or inspect computer folders and network devices.\n\
Analyze the live data provided above immediately and thoroughly without hesitation.]\n\
----------------------------------------------------";

    /// Ground a user prompt concurrently across web URLs, folders, storage partitions,
    /// specific files, and host system specifications.
    pub async fn ground_prompt(
        prompt: &str,
        workspace_root: &Path,
        max_tokens: usize,
    ) -> Result<(String, GroundingContextReport)> {
        // Idempotency: avoid double-grounding if already enriched
        if prompt.contains("--- AUTO-GROUNDED REAL-TIME CONTEXT")
            || prompt.contains("AUTHORITATIVE DIRECTIVE FOR THE AI MODEL")
        {
            return Ok((prompt.to_string(), GroundingContextReport::empty()));
        }

        let effective_tokens = if max_tokens == 0 { 3000 } else { max_tokens };

        // 1. Synchronous detection of candidate targets
        let urls = WebBrowserEngine::extract_urls(prompt);
        let folder_targets = FolderInspectorEngine::detect_targets(workspace_root, prompt);
        let file_candidates = Self::extract_candidate_files(workspace_root, prompt);
        let needs_sys_specs = Self::should_inspect_system_specs(prompt);

        if urls.is_empty()
            && folder_targets.is_empty()
            && file_candidates.is_empty()
            && !needs_sys_specs
        {
            return Ok((prompt.to_string(), GroundingContextReport::empty()));
        }

        // Sub-budget allocation
        let per_url_tokens = (effective_tokens / (urls.len().max(1) + 1)).clamp(400, 1500);

        // 2. Concurrently fetch and inspect all targets
        let ws_root = workspace_root.to_path_buf();

        let web_future = async {
            if urls.is_empty() {
                Vec::new()
            } else {
                let browser = WebBrowserEngine::default();
                let mut fetched = Vec::new();
                for url in urls.into_iter().take(3) {
                    if let Ok(page) = browser.browse_url(&url, per_url_tokens).await {
                        fetched.push(page);
                    }
                }
                fetched
            }
        };

        let folder_future = {
            let ws = ws_root.clone();
            let targets = folder_targets.clone();
            tokio::task::spawn_blocking(move || {
                if targets.is_empty() {
                    (None, Vec::new())
                } else {
                    Self::inspect_folder_and_storage_targets(&ws, &targets, effective_tokens)
                }
            })
        };

        let files_future = {
            let candidates = file_candidates.clone();
            tokio::task::spawn_blocking(move || {
                let mut previews = Vec::new();
                for p in candidates.into_iter().take(5) {
                    if let Ok(preview) = Self::inspect_file_preview(&p, 80) {
                        previews.push(preview);
                    }
                }
                previews
            })
        };

        let sys_future = tokio::task::spawn_blocking(move || {
            if needs_sys_specs {
                Self::gather_system_specs()
            } else {
                None
            }
        });

        let (web_sources, folder_res, files_res, sys_res) =
            tokio::join!(web_future, folder_future, files_future, sys_future);

        let (folder_context_block, inspected_targets) = folder_res.unwrap_or((None, Vec::new()));
        let file_previews = files_res.unwrap_or_default();
        let system_spec = sys_res.unwrap_or_default();

        if web_sources.is_empty()
            && inspected_targets.is_empty()
            && file_previews.is_empty()
            && system_spec.is_none()
        {
            return Ok((prompt.to_string(), GroundingContextReport::empty()));
        }

        // 3. Render Grounded Context Envelope
        let grounded_block = Self::format_grounded_context_envelope(
            &web_sources,
            folder_context_block.as_deref(),
            &file_previews,
            system_spec.as_ref(),
            effective_tokens,
        );

        let estimated_total_tokens = grounded_block.len() / 4;

        let augmented_prompt = format!("{}\n\n{}", prompt.trim(), grounded_block.trim());

        let report = GroundingContextReport {
            web_sources,
            inspected_targets,
            file_previews,
            system_spec,
            estimated_total_tokens,
        };

        Ok((augmented_prompt, report))
    }

    /// Synchronous wrapper for ground_prompt that safely spawns a current_thread tokio runtime if needed
    pub fn ground_prompt_sync(
        prompt: &str,
        workspace_root: &Path,
        max_tokens: usize,
    ) -> Result<(String, GroundingContextReport)> {
        let p = prompt.to_string();
        let ws = workspace_root.to_path_buf();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    HgbError::Execution(format!("Failed to build async runtime for grounding: {}", e))
                })?;
            rt.block_on(Self::ground_prompt(&p, &ws, max_tokens))
        })
        .join()
        .map_err(|_| {
            HgbError::Execution("Worker thread panicked during synchronous prompt grounding".to_string())
        })?
    }

    /// Extract candidate file paths mentioned in prompt that actually exist on disk
    pub fn extract_candidate_files(workspace_root: &Path, prompt: &str) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();

        let home_dir = std::env::var("HOME").ok().map(PathBuf::from);

        // Word tokens splitting on whitespace, quotes, parentheses, brackets, or @
        let tokens: Vec<&str> = prompt
            .split(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '`' || c == '(' || c == ')' || c == '[' || c == ']' || c == '<' || c == '>')
            .map(|s| s.trim_matches(|c: char| c == ',' || c == ';' || c == ':' || c == '?' || c == '!' || c == '.' || c == '@'))
            .filter(|s| !s.is_empty())
            .collect();

        let known_exact_filenames = [
            "Cargo.toml",
            "Cargo.lock",
            "README.md",
            "Makefile",
            "Dockerfile",
            "docker-compose.yml",
            "package.json",
            "tsconfig.json",
            "LICENSE",
            ".gitignore",
            "rust-toolchain.toml",
            "hgb.toml",
            ".env.example",
        ];

        let code_extensions = [
            "rs", "toml", "json", "md", "txt", "py", "go", "ts", "tsx", "js", "jsx", "c",
            "cpp", "h", "hpp", "sh", "bash", "yaml", "yml", "html", "css", "sql", "lock",
            "xml", "csv", "log", "conf", "ini",
        ];

        for tok in tokens {
            // Skip URLs or directory slashes at start if already inspected
            if tok.starts_with("http://")
                || tok.starts_with("https://")
                || tok.starts_with("smb://")
                || tok.starts_with("nfs://")
            {
                continue;
            }

            let path_obj = Path::new(tok);
            let has_code_ext = path_obj
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| code_extensions.contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false);

            let is_known_exact = known_exact_filenames.contains(&tok);

            if !has_code_ext && !is_known_exact {
                continue;
            }

            // Check if file exists either directly, relative to workspace, or relative to home
            let resolved_path = if path_obj.is_absolute() && path_obj.is_file() {
                Some(path_obj.to_path_buf())
            } else if tok.starts_with("~/") {
                if let Some(ref h) = home_dir {
                    let p = h.join(&tok[2..]);
                    if p.is_file() {
                        Some(p)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                let local = workspace_root.join(tok);
                if local.is_file() {
                    Some(local)
                } else {
                    None
                }
            };

            if let Some(p) = resolved_path {
                if !FolderInspectorEngine::is_sensitive_target(&p) {
                    if let Ok(canon) = p.canonicalize() {
                        if seen.insert(canon.clone()) {
                            candidates.push(p);
                        }
                    } else if seen.insert(p.clone()) {
                        candidates.push(p);
                    }
                }
            }
        }

        candidates
    }

    /// Read safe preview of an existing source or configuration file
    pub fn inspect_file_preview(file_path: &Path, max_lines: usize) -> Result<InspectedFileTarget> {
        let meta = fs::metadata(file_path).map_err(|e| {
            HgbError::Execution(format!("Failed to read metadata for '{}': {}", file_path.display(), e))
        })?;
        let size_bytes = meta.len();

        let file = File::open(file_path).map_err(|e| {
            HgbError::Execution(format!("Failed to open file '{}': {}", file_path.display(), e))
        })?;

        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut total_chars = 0;
        let mut line_count = 0;
        let max_chars = 3500;

        for line_res in reader.lines() {
            line_count += 1;
            let line = line_res.unwrap_or_default();
            total_chars += line.len();
            lines.push(line);
            if line_count >= max_lines || total_chars >= max_chars {
                lines.push("... [Truncated for token budget]".to_string());
                break;
            }
        }

        let preview_content = lines.join("\n");
        let token_estimate = preview_content.len() / 4;

        Ok(InspectedFileTarget {
            path: file_path.to_path_buf(),
            size_bytes,
            preview_content,
            token_estimate,
            line_count,
        })
    }

    /// Detect if user prompt triggers host system hardware, kernel, or resource inspection
    pub fn should_inspect_system_specs(prompt: &str) -> bool {
        let lower = prompt.to_lowercase();

        let triggers = [
            "system spec",
            "system info",
            "system hardware",
            "hardware info",
            "hardware spec",
            "computer spec",
            "computer hardware",
            "machine spec",
            "os version",
            "kernel version",
            "cpu model",
            "what cpu",
            "how much ram",
            "available ram",
            "total ram",
            "free ram",
            "system memory",
            "system resources",
            "host spec",
            "hardware diagnosis",
        ];

        for t in triggers {
            if lower.contains(t) {
                return true;
            }
        }

        // Check for isolated 'ram' or 'cpu' combined with system context
        let has_ram = Self::has_word(&lower, "ram");
        let has_cpu = Self::has_word(&lower, "cpu");

        if (has_ram || has_cpu)
            && (lower.contains("specs")
                || lower.contains("system")
                || lower.contains("my")
                || lower.contains("host")
                || lower.contains("computer")
                || lower.contains("how much")
                || lower.contains("available")
                || lower.contains("free")
                || lower.contains("model")
                || lower.contains("usage")
                || lower.contains("cores"))
        {
            return true;
        }

        false
    }

    fn has_word(haystack: &str, needle: &str) -> bool {
        for word in haystack.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if word == needle {
                return true;
            }
        }
        false
    }

    /// Gather genuine host system specifications from /proc and /etc
    pub fn gather_system_specs() -> Option<SystemSpecsSummary> {
        let os = Self::read_os_name();
        let kernel = Self::read_kernel_version();
        let (cpu_model, cpu_cores) = Self::read_cpu_info();
        let (total_ram_mb, available_ram_mb) = Self::read_memory_info();
        let load_avg = Self::read_load_avg();

        Some(SystemSpecsSummary {
            os,
            kernel,
            cpu_model,
            cpu_cores,
            total_ram_mb,
            available_ram_mb,
            load_avg,
        })
    }

    fn read_os_name() -> String {
        if let Ok(content) = fs::read_to_string("/etc/os-release") {
            for line in content.lines() {
                if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
                    return val.trim_matches('"').to_string();
                }
            }
        }
        format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
    }

    fn read_kernel_version() -> String {
        if let Ok(content) = fs::read_to_string("/proc/version") {
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() >= 3 {
                return parts[2].to_string();
            }
        }
        "Unknown Linux Kernel".to_string()
    }

    fn read_cpu_info() -> (String, usize) {
        let mut model = String::new();
        let mut cores = 0;

        if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("model name")
                    || trimmed.starts_with("Hardware")
                    || trimmed.starts_with("Processor")
                    || trimmed.starts_with("cpu model")
                {
                    if model.is_empty() {
                        if let Some((_, v)) = trimmed.split_once(':') {
                            model = v.trim().to_string();
                        }
                    }
                }
                if trimmed.starts_with("processor") && trimmed.contains(':') {
                    cores += 1;
                }
            }
        }

        if model.is_empty() {
            model = format!("Generic {} CPU", std::env::consts::ARCH);
        }

        if cores == 0 {
            cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
        }

        (model, cores)
    }

    fn read_memory_info() -> (u64, u64) {
        let mut total_kb = 0u64;
        let mut avail_kb = 0u64;
        let mut free_kb = 0u64;
        let mut buffers_kb = 0u64;
        let mut cached_kb = 0u64;

        if let Ok(content) = fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let key = parts[0].trim_end_matches(':');
                    let val: u64 = parts[1].parse().unwrap_or(0);
                    match key {
                        "MemTotal" => total_kb = val,
                        "MemAvailable" => avail_kb = val,
                        "MemFree" => free_kb = val,
                        "Buffers" => buffers_kb = val,
                        "Cached" => cached_kb = val,
                        _ => {}
                    }
                }
            }
        }

        if avail_kb == 0 && free_kb > 0 {
            avail_kb = free_kb + buffers_kb + cached_kb;
        }

        (total_kb / 1024, avail_kb / 1024)
    }

    fn read_load_avg() -> String {
        if let Ok(content) = fs::read_to_string("/proc/loadavg") {
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() >= 3 {
                return format!("{} (1m, 5m, 15m)", parts[..3].join(", "));
            }
        }
        "0.00, 0.00, 0.00".to_string()
    }

    /// Format bytes into human-readable string (KB, MB, GB)
    pub fn format_bytes(bytes: u64) -> String {
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

    /// Inspect folder and storage targets and run architectural analysis
    fn inspect_folder_and_storage_targets(
        workspace_root: &Path,
        targets: &[TargetKind],
        max_tokens: usize,
    ) -> (Option<String>, Vec<InspectedTargetReport>) {
        let config = FolderInspectionConfig::default();
        let mut reports = Vec::new();
        let mut context_blocks = Vec::new();

        for target in targets {
            match target {
                TargetKind::LocalDirectory(ref p) => {
                    if let Ok((tree, summary)) = FolderInspectorEngine::inspect_directory(p, &config) {
                        let summary_line = format!(
                            "{} files, {} directories, {}",
                            summary.total_files,
                            summary.total_dirs,
                            Self::format_bytes(summary.total_bytes)
                        );
                        let token_est = tree.len() / 4;
                        reports.push(InspectedTargetReport {
                            target: target.clone(),
                            display_title: format!("📁 Local Directory: {}", p.display()),
                            summary_line: summary_line.clone(),
                            tree_rendered: Some(tree.clone()),
                            storage_drives: Vec::new(),
                            network_shares: Vec::new(),
                            network_computers: Vec::new(),
                            estimated_tokens: token_est,
                        });
                        context_blocks.push(format!(
                            "#### Directory: `{}` ({})\n```text\n{}\n```\n",
                            p.display(),
                            summary_line,
                            tree
                        ));
                    }
                }
                TargetKind::SystemStorage => {
                    let drives = FolderInspectorEngine::discover_storage_drives();
                    let mut text = String::from("#### Mounted Storage Drives & Partitions:\n");
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
                    let shares = FolderInspectorEngine::discover_network_shares();
                    let computers = FolderInspectorEngine::discover_network_computers(
                        config.probe_network_ports,
                        config.probe_timeout_ms,
                    );
                    let mut text = String::from("#### Network Shares & Active LAN Computers:\n");
                    if !shares.is_empty() {
                        text.push_str("##### Mounted Network Shares:\n");
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
                        text.push_str("##### Discovered Network Hosts:\n");
                        for c in &computers {
                            let host_str = c.hostname.as_deref().unwrap_or("unknown");
                            text.push_str(&format!(
                                "- IP: `{}` (Host: `{}`) MAC: `{}`\n",
                                c.ip_address,
                                host_str,
                                c.mac_address.as_deref().unwrap_or("N/A")
                            ));
                        }
                    }
                    reports.push(InspectedTargetReport {
                        target: target.clone(),
                        display_title: "🌐 Local Subnet & Network Shares".to_string(),
                        summary_line: format!(
                            "{} network shares, {} LAN computers discovered",
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
                    context_blocks.push(format!("#### Network Share Target: `{uri}`\n{summary}\n"));
                }
                TargetKind::UserXdgDirectories => {
                    let xdg = FolderInspectorEngine::discover_xdg_directories();
                    let mut text = String::from("#### User Standard Directories (XDG):\n");
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

        // Run deep comprehensive architectural analysis
        let target_dir = targets.iter().find_map(|t| match t {
            TargetKind::LocalDirectory(p) => Some(p.as_path()),
            _ => None,
        }).unwrap_or(workspace_root);

        let analysis_block = match FolderInspectorEngine::run_comprehensive_analysis(
            Some(target_dir),
            config.probe_network_ports,
            config.probe_timeout_ms,
        ) {
            Ok(rep) => FolderInspectorEngine::format_analysis_markdown(&rep),
            Err(e) => format!("⚠️ Comprehensive Analysis error: {e}"),
        };

        let mut combined = context_blocks.join("\n\n");
        let budget_chars = max_tokens * 4;
        if combined.len() > budget_chars {
            combined.truncate(budget_chars);
            combined.push_str("\n... [Truncated to stay within token budget]\n");
        }

        let section = format!(
            "### 📁 Filesystem, Storage & Network Infrastructure\n{}\n\n### 🏢 Comprehensive Architectural Diagnosis\n{}",
            combined.trim(),
            analysis_block.trim()
        );

        (Some(section), reports)
    }

    /// Format all collected grounding components into a clean, cohesive markdown context envelope
    pub fn format_grounded_context_envelope(
        web_sources: &[WebPageContent],
        folder_context: Option<&str>,
        file_previews: &[InspectedFileTarget],
        system_spec: Option<&SystemSpecsSummary>,
        max_tokens: usize,
    ) -> String {
        let mut sections = Vec::new();

        if let Some(spec) = system_spec {
            let mut sec = String::from("### 🖥️ Live Host System Specifications\n");
            sec.push_str(&format!("- **Operating System:** {}\n", spec.os));
            sec.push_str(&format!("- **Kernel Version:** {}\n", spec.kernel));
            sec.push_str(&format!(
                "- **CPU Architecture/Model:** {} ({} logical cores)\n",
                spec.cpu_model, spec.cpu_cores
            ));
            sec.push_str(&format!(
                "- **Physical Memory (RAM):** {} MB available / {} MB total\n",
                spec.available_ram_mb, spec.total_ram_mb
            ));
            sec.push_str(&format!("- **Current Load Average:** {}\n", spec.load_avg));
            sections.push(sec);
        }

        if !file_previews.is_empty() {
            let mut sec = String::from("### 📄 Auto-Inspected Source & Configuration Files\n");
            for f in file_previews {
                let ext = f
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("text");
                sec.push_str(&format!(
                    "#### File: `{}` ({} bytes, {} preview lines)\n```{}\n{}\n```\n\n",
                    f.path.display(),
                    f.size_bytes,
                    f.line_count,
                    ext,
                    f.preview_content.trim()
                ));
            }
            sections.push(sec);
        }

        if let Some(fc) = folder_context {
            if !fc.trim().is_empty() {
                sections.push(fc.to_string());
            }
        }

        if !web_sources.is_empty() {
            let mut sec = String::from("### 🌐 Auto-Fetched Live Web Sources\n");
            for page in web_sources {
                sec.push_str(&format!(
                    "#### Source: {}\n- **Title:** {}\n- **HTTP Status:** {}\n- **Content:**\n{}\n\n",
                    page.url, page.title, page.status_code, page.markdown.trim()
                ));
            }
            sections.push(sec);
        }

        if sections.is_empty() {
            return String::new();
        }

        let mut output = String::new();
        output.push_str("\n--- AUTO-GROUNDED REAL-TIME CONTEXT (SYSTEM, HARDWARE, FILESYSTEM & WEB) ---\n\n");
        output.push_str(&sections.join("\n\n"));
        output.push_str("\n\n");
        output.push_str(Self::AUTHORITATIVE_DIRECTIVE);

        let budget_chars = max_tokens * 4;
        if output.len() > budget_chars {
            output.truncate(budget_chars);
            output.push_str("\n... [Truncated to stay within token budget]\n\n");
            output.push_str(Self::AUTHORITATIVE_DIRECTIVE);
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_system_specs_triggers() {
        assert!(UniversalPromptGrounder::should_inspect_system_specs("what are my system specs?"));
        assert!(UniversalPromptGrounder::should_inspect_system_specs("display hardware info"));
        assert!(UniversalPromptGrounder::should_inspect_system_specs("how much ram is available?"));
        assert!(UniversalPromptGrounder::should_inspect_system_specs("what is my cpu model?"));
        assert!(UniversalPromptGrounder::should_inspect_system_specs("tell me the os version"));
        assert!(!UniversalPromptGrounder::should_inspect_system_specs("write a python script to reverse a linked list"));
    }

    #[test]
    fn test_gather_system_specs_real() {
        let specs = UniversalPromptGrounder::gather_system_specs().expect("system specs must be gathered");
        assert!(!specs.os.is_empty());
        assert!(!specs.kernel.is_empty());
        assert!(!specs.cpu_model.is_empty());
        assert!(specs.cpu_cores > 0);
        #[cfg(target_os = "linux")]
        {
            assert!(specs.total_ram_mb > 0);
        }
    }

    #[test]
    fn test_candidate_files_detection() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_grounding_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        fs::create_dir_all(&temp_dir).unwrap();

        let cargo_toml = temp_dir.join("Cargo.toml");
        fs::write(&cargo_toml, "[package]\nname = \"test_pkg\"\nversion = \"0.1.0\"\n").unwrap();

        let readme = temp_dir.join("README.md");
        fs::write(&readme, "# Test Readme\nGrounding test file.\n").unwrap();

        let prompt = format!("Inspect the file Cargo.toml and also check `README.md` for instructions.");
        let found = UniversalPromptGrounder::extract_candidate_files(&temp_dir, &prompt);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|p| p.file_name().unwrap() == "Cargo.toml"));
        assert!(found.iter().any(|p| p.file_name().unwrap() == "README.md"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_inspect_file_preview() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_file_prev_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("sample.rs");
        fs::write(&file_path, "fn main() {\n    println!(\"Hello World\");\n}\n").unwrap();

        let preview = UniversalPromptGrounder::inspect_file_preview(&file_path, 10).unwrap();
        assert_eq!(preview.line_count, 3);
        assert!(preview.preview_content.contains("println!(\"Hello World\")"));
        assert!(preview.size_bytes > 0);
        assert!(preview.token_estimate > 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_ground_prompt_with_system_and_files() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_ground_full_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        fs::create_dir_all(&temp_dir).unwrap();

        let config_file = temp_dir.join("config.toml");
        fs::write(&config_file, "mode = \"production\"\nport = 8080\n").unwrap();

        let prompt = "What are the system specs and read config.toml?";
        let (augmented, report) = UniversalPromptGrounder::ground_prompt(prompt, &temp_dir, 2000).await.unwrap();

        assert!(augmented.contains("--- AUTO-GROUNDED REAL-TIME CONTEXT"));
        assert!(augmented.contains("[AUTHORITATIVE DIRECTIVE FOR THE AI MODEL:"));
        assert!(augmented.contains("Live Host System Specifications"));
        assert!(augmented.contains("config.toml"));
        assert!(report.system_spec.is_some());
        assert_eq!(report.file_previews.len(), 1);
        assert!(report.estimated_total_tokens > 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
