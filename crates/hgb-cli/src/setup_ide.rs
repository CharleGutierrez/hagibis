//! 1-Click IDE Onboarding & MCP Configuration Setup (`hgb setup`)
//!
//! Generates and non-destructively merges MCP configuration files for:
//! - Cursor: `.cursor/mcp.json`
//! - VS Code: `.vscode/settings.json`
//! - Windsurf: `.codeium/windsurf/mcp_config.json`
//!
//! All updates preserve existing user configurations, custom tools, and non-Hagibis MCP servers.

use std::fs;
use std::path::{Path, PathBuf};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_SSE_URL: &str = "http://127.0.0.1:9099/sse";
pub const DEFAULT_MCP_COMMAND: &str = "hgb";

/// Command line arguments for `hgb setup`
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct SetupArgs {
    /// Configure Cursor IDE (.cursor/mcp.json)
    #[arg(long)]
    pub cursor: bool,

    /// Configure VS Code (.vscode/settings.json)
    #[arg(long)]
    pub vscode: bool,

    /// Configure Windsurf IDE (.codeium/windsurf/mcp_config.json)
    #[arg(long)]
    pub windsurf: bool,

    /// Configure all supported IDEs (Cursor, VS Code, Windsurf)
    #[arg(long)]
    pub all: bool,

    /// Base workspace directory (defaults to current directory)
    #[arg(short, long)]
    pub path: Option<PathBuf>,

    /// Hagibis SSE URL (default: http://127.0.0.1:9099/sse)
    #[arg(long, default_value = DEFAULT_SSE_URL)]
    pub sse_url: String,

    /// Hagibis CLI binary command for stdio MCP (default: hgb)
    #[arg(long, default_value = DEFAULT_MCP_COMMAND)]
    pub command: String,
}

/// Report of IDE onboarding action
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdeSetupResult {
    pub target_ide: String,
    pub config_path: PathBuf,
    pub created_new: bool,
    pub merged_existing: bool,
    pub success: bool,
    pub message: String,
}

/// Deeply merge `source` JSON object into `target` JSON object.
/// Preserves existing keys in `target` while updating or appending keys from `source`.
pub fn deep_merge_json(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, val) in source_map {
                if let Some(target_val) = target_map.get_mut(key) {
                    deep_merge_json(target_val, val);
                } else {
                    target_map.insert(key.clone(), val.clone());
                }
            }
        }
        (target_slot, source_val) => {
            *target_slot = source_val.clone();
        }
    }
}

/// Read existing JSON file or initialize empty JSON object, ensuring parent directories exist.
fn load_or_init_json_object(path: &Path) -> std::io::Result<(Value, bool)> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if path.exists() {
        let content = fs::read_to_string(path)?;
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&content) {
            return Ok((Value::Object(map), false));
        }
    }

    Ok((json!({}), true))
}

/// Write formatted JSON to file
fn write_pretty_json(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, format!("{}\n", content))
}

/// IDE Onboarding Engine
pub struct IdeOnboardingEngine;

impl IdeOnboardingEngine {
    /// Configure Cursor IDE MCP: `.cursor/mcp.json`
    ///
    /// Cursor format:
    /// ```json
    /// {
    ///   "mcpServers": {
    ///     "hagibis": { "command": "hgb", "args": ["mcp"] },
    ///     "hagibis-sse": { "url": "http://127.0.0.1:9099/sse" }
    ///   }
    /// }
    /// ```
    pub fn setup_cursor(workspace_root: &Path, sse_url: &str, command: &str) -> std::io::Result<IdeSetupResult> {
        let config_path = workspace_root.join(".cursor").join("mcp.json");
        let (mut current_json, created_new) = load_or_init_json_object(&config_path)?;

        let cursor_patch = json!({
            "mcpServers": {
                "hagibis": {
                    "command": command,
                    "args": ["mcp"]
                },
                "hagibis-sse": {
                    "url": sse_url
                }
            }
        });

        deep_merge_json(&mut current_json, &cursor_patch);
        write_pretty_json(&config_path, &current_json)?;

        Ok(IdeSetupResult {
            target_ide: "Cursor".to_string(),
            config_path,
            created_new,
            merged_existing: !created_new,
            success: true,
            message: if created_new {
                "Generated Cursor MCP configuration (.cursor/mcp.json)".to_string()
            } else {
                "Merged Hagibis MCP servers into existing .cursor/mcp.json".to_string()
            },
        })
    }

    /// Configure VS Code MCP: `.vscode/settings.json`
    ///
    /// Sets both `mcpServers` and `mcp.servers` keys to support all VS Code MCP extensions and Copilot.
    pub fn setup_vscode(workspace_root: &Path, sse_url: &str, command: &str) -> std::io::Result<IdeSetupResult> {
        let config_path = workspace_root.join(".vscode").join("settings.json");
        let (mut current_json, created_new) = load_or_init_json_object(&config_path)?;

        let vscode_patch = json!({
            "mcpServers": {
                "hagibis": {
                    "command": command,
                    "args": ["mcp"]
                },
                "hagibis-sse": {
                    "url": sse_url
                }
            },
            "mcp.servers": {
                "hagibis": {
                    "command": command,
                    "args": ["mcp"]
                },
                "hagibis-sse": {
                    "url": sse_url
                }
            }
        });

        deep_merge_json(&mut current_json, &vscode_patch);
        write_pretty_json(&config_path, &current_json)?;

        Ok(IdeSetupResult {
            target_ide: "VS Code".to_string(),
            config_path,
            created_new,
            merged_existing: !created_new,
            success: true,
            message: if created_new {
                "Generated VS Code settings (.vscode/settings.json)".to_string()
            } else {
                "Merged Hagibis MCP configuration into .vscode/settings.json".to_string()
            },
        })
    }

    /// Configure Windsurf IDE MCP: `.codeium/windsurf/mcp_config.json`
    ///
    /// Windsurf format:
    /// ```json
    /// {
    ///   "mcpServers": {
    ///     "hagibis": { "command": "hgb", "args": ["mcp"] },
    ///     "hagibis-sse": { "serverUrl": "http://127.0.0.1:9099/sse" }
    ///   }
    /// }
    /// ```
    pub fn setup_windsurf(workspace_root: &Path, sse_url: &str, command: &str) -> std::io::Result<IdeSetupResult> {
        let config_path = workspace_root.join(".codeium").join("windsurf").join("mcp_config.json");
        let (mut current_json, created_new) = load_or_init_json_object(&config_path)?;

        let windsurf_patch = json!({
            "mcpServers": {
                "hagibis": {
                    "command": command,
                    "args": ["mcp"]
                },
                "hagibis-sse": {
                    "serverUrl": sse_url,
                    "url": sse_url
                }
            }
        });

        deep_merge_json(&mut current_json, &windsurf_patch);
        write_pretty_json(&config_path, &current_json)?;

        Ok(IdeSetupResult {
            target_ide: "Windsurf".to_string(),
            config_path,
            created_new,
            merged_existing: !created_new,
            success: true,
            message: if created_new {
                "Generated Windsurf MCP configuration (.codeium/windsurf/mcp_config.json)".to_string()
            } else {
                "Merged Hagibis MCP servers into .codeium/windsurf/mcp_config.json".to_string()
            },
        })
    }

    /// Configure all supported IDEs (Cursor, VS Code, Windsurf)
    pub fn setup_all(workspace_root: &Path, sse_url: &str, command: &str) -> std::io::Result<Vec<IdeSetupResult>> {
        let mut results = Vec::new();
        results.push(Self::setup_cursor(workspace_root, sse_url, command)?);
        results.push(Self::setup_vscode(workspace_root, sse_url, command)?);
        results.push(Self::setup_windsurf(workspace_root, sse_url, command)?);
        Ok(results)
    }
}

/// Execute onboarding according to `SetupArgs`
pub fn run_setup(workspace_root: &Path, args: &SetupArgs) -> std::io::Result<Vec<IdeSetupResult>> {
    let mut results = Vec::new();

    let target_all = args.all || (!args.cursor && !args.vscode && !args.windsurf);

    if target_all || args.cursor {
        results.push(IdeOnboardingEngine::setup_cursor(workspace_root, &args.sse_url, &args.command)?);
    }
    if target_all || args.vscode {
        results.push(IdeOnboardingEngine::setup_vscode(workspace_root, &args.sse_url, &args.command)?);
    }
    if target_all || args.windsurf {
        results.push(IdeOnboardingEngine::setup_windsurf(workspace_root, &args.sse_url, &args.command)?);
    }

    Ok(results)
}

pub const MCP_SSE_DEFAULT_URL: &str = "http://127.0.0.1:8083/sse";

/// Target IDE selection for `hgb mcp install-ide`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeTarget {
    Cursor,
    Windsurf,
    Claude,
    All,
}

impl std::str::FromStr for IdeTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "cursor" => Ok(IdeTarget::Cursor),
            "windsurf" => Ok(IdeTarget::Windsurf),
            "claude" | "claude-desktop" | "claudedesktop" => Ok(IdeTarget::Claude),
            "all" => Ok(IdeTarget::All),
            other => Err(format!(
                "Unknown IDE target '{}'. Supported targets: cursor, windsurf, claude, all",
                other
            )),
        }
    }
}

/// Metadata and status report of an installed MCP IDE configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledIdeInfo {
    pub name: String,
    pub path: PathBuf,
    pub sse_url: String,
    pub created: bool,
    pub instructions: String,
}

/// Resolve config path for Cursor IDE (.cursor/mcp.json)
pub fn resolve_cursor_config_path(workspace_root: &Path) -> PathBuf {
    let ws_path = workspace_root.join(".cursor").join("mcp.json");
    if ws_path.exists() {
        return ws_path;
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    let home_path = home.join(".cursor").join("mcp.json");
    if home_path.exists() {
        return home_path;
    }
    if workspace_root.join(".cursor").is_dir() {
        return ws_path;
    }
    home_path
}

/// Resolve config path for Windsurf IDE (.codeium/windsurf/mcp_config.json)
pub fn resolve_windsurf_config_path() -> PathBuf {
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    home.join(".codeium").join("windsurf").join("mcp_config.json")
}

/// Resolve config path for Claude Desktop
pub fn resolve_claude_config_path() -> PathBuf {
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    {
        let mac_path = home.join("Library").join("Application Support").join("Claude").join("claude_desktop_config.json");
        if mac_path.exists() || !home.join(".config").join("Claude").join("claude_desktop_config.json").exists() {
            return mac_path;
        }
    }
    home.join(".config").join("Claude").join("claude_desktop_config.json")
}

/// Insert or update the `"hagibis"` server definition under `"mcpServers"` in a target JSON file:
/// ```json
/// "hagibis": {
///   "url": "http://127.0.0.1:8083/sse",
///   "headers": {
///     "Authorization": "Bearer <TOKEN>"
///   }
/// }
/// ```
pub fn install_mcp_to_config(config_path: &Path, sse_url: &str, token: &str) -> std::io::Result<(bool, Value)> {
    let (mut root, created) = load_or_init_json_object(config_path)?;
    if !root.is_object() {
        root = json!({});
    }

    let root_obj = root.as_object_mut().unwrap();
    let mcp_servers = root_obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}));

    if !mcp_servers.is_object() {
        *mcp_servers = json!({});
    }

    let server_entry = json!({
        "url": sse_url,
        "headers": {
            "Authorization": format!("Bearer {}", token)
        }
    });

    mcp_servers
        .as_object_mut()
        .unwrap()
        .insert("hagibis".to_string(), server_entry);

    write_pretty_json(config_path, &root)?;
    Ok((created, root))
}

/// Configure Hagibis MCP SSE endpoint into target IDE configs
pub fn install_ide_mcp(
    target: IdeTarget,
    workspace_root: Option<&Path>,
    token: &str,
    sse_url: Option<&str>,
) -> std::io::Result<Vec<InstalledIdeInfo>> {
    let url = sse_url.unwrap_or(MCP_SSE_DEFAULT_URL);
    let ws = workspace_root.unwrap_or_else(|| Path::new("."));
    let mut results = Vec::new();

    let targets = match target {
        IdeTarget::All => vec![IdeTarget::Cursor, IdeTarget::Windsurf, IdeTarget::Claude],
        other => vec![other],
    };

    for t in targets {
        match t {
            IdeTarget::Cursor => {
                let path = resolve_cursor_config_path(ws);
                let (created, _) = install_mcp_to_config(&path, url, token)?;
                results.push(InstalledIdeInfo {
                    name: "Cursor".to_string(),
                    path,
                    sse_url: url.to_string(),
                    created,
                    instructions: "Open Cursor Settings > Features > MCP, or restart Cursor to activate Hagibis MCP tools.".to_string(),
                });
            }
            IdeTarget::Windsurf => {
                let path = resolve_windsurf_config_path();
                let (created, _) = install_mcp_to_config(&path, url, token)?;
                results.push(InstalledIdeInfo {
                    name: "Windsurf".to_string(),
                    path,
                    sse_url: url.to_string(),
                    created,
                    instructions: "Restart Windsurf or reload Cascade Plugins to establish connection with Hagibis SSE.".to_string(),
                });
            }
            IdeTarget::Claude => {
                let path = resolve_claude_config_path();
                let (created, _) = install_mcp_to_config(&path, url, token)?;
                results.push(InstalledIdeInfo {
                    name: "Claude Desktop".to_string(),
                    path,
                    sse_url: url.to_string(),
                    created,
                    instructions: "Fully restart Claude Desktop (Quit from tray/menu and relaunch) to register Hagibis MCP.".to_string(),
                });
            }
            IdeTarget::All => unreachable!(),
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_cursor_config_generation_from_scratch() {
        let temp = tempdir().unwrap();
        let ws = temp.path();

        let res = IdeOnboardingEngine::setup_cursor(ws, "http://127.0.0.1:9099/sse", "hgb").unwrap();
        assert!(res.success);
        assert!(res.created_new);
        assert!(!res.merged_existing);
        assert!(res.config_path.exists());

        let content = fs::read_to_string(&res.config_path).unwrap();
        let val: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(val["mcpServers"]["hagibis"]["command"], "hgb");
        assert_eq!(val["mcpServers"]["hagibis"]["args"][0], "mcp");
        assert_eq!(val["mcpServers"]["hagibis-sse"]["url"], "http://127.0.0.1:9099/sse");
    }

    #[test]
    fn test_cursor_json_merging_preserves_existing_servers() {
        let temp = tempdir().unwrap();
        let ws = temp.path();
        let cursor_dir = ws.join(".cursor");
        fs::create_dir_all(&cursor_dir).unwrap();

        let initial_json = json!({
            "mcpServers": {
                "custom_tool": {
                    "command": "custom_mcp",
                    "args": ["--port", "4000"]
                }
            },
            "customSetting": true
        });
        fs::write(cursor_dir.join("mcp.json"), serde_json::to_string_pretty(&initial_json).unwrap()).unwrap();

        let res = IdeOnboardingEngine::setup_cursor(ws, "http://127.0.0.1:9099/sse", "hgb").unwrap();
        assert!(res.success);
        assert!(!res.created_new);
        assert!(res.merged_existing);

        let content = fs::read_to_string(&res.config_path).unwrap();
        let val: Value = serde_json::from_str(&content).unwrap();

        // Preserved custom server & root property
        assert_eq!(val["mcpServers"]["custom_tool"]["command"], "custom_mcp");
        assert_eq!(val["customSetting"], true);
        // Added hagibis
        assert_eq!(val["mcpServers"]["hagibis"]["command"], "hgb");
        assert_eq!(val["mcpServers"]["hagibis-sse"]["url"], "http://127.0.0.1:9099/sse");
    }

    #[test]
    fn test_vscode_config_generation_and_merge() {
        let temp = tempdir().unwrap();
        let ws = temp.path();
        let vscode_dir = ws.join(".vscode");
        fs::create_dir_all(&vscode_dir).unwrap();

        let initial_json = json!({
            "editor.fontSize": 14,
            "editor.tabSize": 4
        });
        fs::write(vscode_dir.join("settings.json"), serde_json::to_string_pretty(&initial_json).unwrap()).unwrap();

        let res = IdeOnboardingEngine::setup_vscode(ws, "http://127.0.0.1:9099/sse", "hgb").unwrap();
        assert!(res.success);
        assert!(res.merged_existing);

        let content = fs::read_to_string(&res.config_path).unwrap();
        let val: Value = serde_json::from_str(&content).unwrap();

        // Preserved editor settings
        assert_eq!(val["editor.fontSize"], 14);
        assert_eq!(val["editor.tabSize"], 4);
        // Added MCP
        assert_eq!(val["mcpServers"]["hagibis"]["command"], "hgb");
        assert_eq!(val["mcp.servers"]["hagibis-sse"]["url"], "http://127.0.0.1:9099/sse");
    }

    #[test]
    fn test_windsurf_config_generation_and_merge() {
        let temp = tempdir().unwrap();
        let ws = temp.path();

        let res = IdeOnboardingEngine::setup_windsurf(ws, "http://127.0.0.1:9099/sse", "hgb").unwrap();
        assert!(res.success);
        assert!(res.created_new);
        assert!(res.config_path.ends_with(".codeium/windsurf/mcp_config.json"));

        let content = fs::read_to_string(&res.config_path).unwrap();
        let val: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(val["mcpServers"]["hagibis"]["command"], "hgb");
        assert_eq!(val["mcpServers"]["hagibis-sse"]["serverUrl"], "http://127.0.0.1:9099/sse");
    }

    #[test]
    fn test_run_setup_all_flag() {
        let temp = tempdir().unwrap();
        let ws = temp.path();

        let args = SetupArgs {
            cursor: false,
            vscode: false,
            windsurf: false,
            all: true,
            path: Some(ws.to_path_buf()),
            sse_url: "http://127.0.0.1:9099/sse".to_string(),
            command: "hgb".to_string(),
        };

        let results = run_setup(ws, &args).unwrap();
        assert_eq!(results.len(), 3);
        assert!(ws.join(".cursor/mcp.json").exists());
        assert!(ws.join(".vscode/settings.json").exists());
        assert!(ws.join(".codeium/windsurf/mcp_config.json").exists());
    }

    #[test]
    fn test_deep_merge_complex_nested_objects() {
        let mut target = json!({
            "a": 1,
            "nested": {
                "x": "keep",
                "y": 10
            }
        });

        let source = json!({
            "b": 2,
            "nested": {
                "y": 20,
                "z": "new"
            }
        });

        deep_merge_json(&mut target, &source);

        assert_eq!(target["a"], 1);
        assert_eq!(target["b"], 2);
        assert_eq!(target["nested"]["x"], "keep");
        assert_eq!(target["nested"]["y"], 20);
        assert_eq!(target["nested"]["z"], "new");
    }

    #[test]
    fn test_ide_target_parsing() {
        assert_eq!("cursor".parse::<IdeTarget>().unwrap(), IdeTarget::Cursor);
        assert_eq!("Cursor".parse::<IdeTarget>().unwrap(), IdeTarget::Cursor);
        assert_eq!("windsurf".parse::<IdeTarget>().unwrap(), IdeTarget::Windsurf);
        assert_eq!("claude".parse::<IdeTarget>().unwrap(), IdeTarget::Claude);
        assert_eq!("claude-desktop".parse::<IdeTarget>().unwrap(), IdeTarget::Claude);
        assert_eq!("all".parse::<IdeTarget>().unwrap(), IdeTarget::All);
        assert!("invalid".parse::<IdeTarget>().is_err());
    }

    #[test]
    fn test_install_mcp_to_config_creates_fresh_file() {
        let temp = tempdir().unwrap();
        let cfg = temp.path().join("sub").join("mcp.json");
        let token = "a1b2c3d4e5f60718293a4b5c6d7e8f90";

        let (created, val) = install_mcp_to_config(&cfg, "http://127.0.0.1:8083/sse", token).unwrap();
        assert!(created);
        assert!(cfg.exists());

        assert_eq!(val["mcpServers"]["hagibis"]["url"], "http://127.0.0.1:8083/sse");
        assert_eq!(
            val["mcpServers"]["hagibis"]["headers"]["Authorization"],
            format!("Bearer {}", token)
        );
    }

    #[test]
    fn test_install_mcp_to_config_preserves_existing_content() {
        let temp = tempdir().unwrap();
        let cfg = temp.path().join("mcp.json");
        let initial = json!({
            "theme": "dark",
            "mcpServers": {
                "sqlite": {
                    "command": "mcp-sqlite",
                    "args": ["--db", "test.db"]
                }
            }
        });
        fs::write(&cfg, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let token = "00112233445566778899aabbccddeeff";
        let (created, val) = install_mcp_to_config(&cfg, "http://127.0.0.1:8083/sse", token).unwrap();
        assert!(!created);

        // Verify existing servers and top-level fields are intact
        assert_eq!(val["theme"], "dark");
        assert_eq!(val["mcpServers"]["sqlite"]["command"], "mcp-sqlite");
        // Verify hagibis is correctly configured
        assert_eq!(val["mcpServers"]["hagibis"]["url"], "http://127.0.0.1:8083/sse");
        assert_eq!(
            val["mcpServers"]["hagibis"]["headers"]["Authorization"],
            format!("Bearer {}", token)
        );
    }
}
