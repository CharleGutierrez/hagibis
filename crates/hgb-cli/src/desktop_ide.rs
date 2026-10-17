//! # Standalone Desktop IDE Launcher (`hgb desktop-ide`, `hgb ide --desktop`)
//!
//! Spawns the sovereign Hagibis PWA Studio as a dedicated, borderless desktop application window:
//! 1. Discovers local Chromium-based runtimes (`google-chrome`, `chromium`, `brave`, `edge`).
//! 2. Launches clean standalone window mode (`--app=http://127.0.0.1:<port> --window-size=1400,900`).
//! 3. Creates an isolated profile at `~/.hgb/desktop_profile/` preventing cookie or extension interference.
//! 4. Seamless fallback to `xdg-open` on Linux if no Chromium browser binary is located.

use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

/// Configuration options for the Desktop IDE launcher
#[derive(Debug, Clone)]
pub struct DesktopLaunchConfig {
    pub port: u16,
    pub width: u32,
    pub height: u32,
    pub custom_browser: Option<String>,
    pub isolated_profile: bool,
}

impl Default for DesktopLaunchConfig {
    fn default() -> Self {
        Self {
            port: 38472,
            width: 1400,
            height: 900,
            custom_browser: None,
            isolated_profile: true,
        }
    }
}

/// Status report returned after launching Desktop IDE
#[derive(Debug, Clone)]
pub struct DesktopLaunchReport {
    pub url: String,
    pub browser_binary: String,
    pub is_app_mode: bool,
    pub profile_dir: Option<PathBuf>,
}

/// Standalone Desktop IDE application launcher
pub struct DesktopIdeLauncher {
    config: DesktopLaunchConfig,
}

impl DesktopIdeLauncher {
    pub fn new(config: Option<DesktopLaunchConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
        }
    }

    /// List of candidate browser binaries supporting `--app` mode
    const CANDIDATE_BROWSERS: &'static [&'static str] = &[
        "google-chrome-stable",
        "google-chrome",
        "chromium-browser",
        "chromium",
        "brave-browser",
        "brave",
        "microsoft-edge-stable",
        "microsoft-edge",
    ];

    /// Locate first available Chromium-based browser on host system
    pub fn find_app_browser() -> Option<String> {
        for candidate in Self::CANDIDATE_BROWSERS {
            if let Ok(output) = Command::new("which").arg(candidate).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// Launch standalone desktop IDE window
    pub fn launch(&self) -> std::io::Result<DesktopLaunchReport> {
        let url = format!("http://127.0.0.1:{}", self.config.port);

        let profile_dir = if self.config.isolated_profile {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let dir = PathBuf::from(home).join(".hgb").join("desktop_profile");
            let _ = fs::create_dir_all(&dir);
            Some(dir)
        } else {
            None
        };

        // Determine browser to execute
        let browser_cmd = self
            .config
            .custom_browser
            .clone()
            .or_else(Self::find_app_browser);

        if let Some(browser) = browser_cmd {
            let mut cmd = Command::new(&browser);
            cmd.arg(format!("--app={}", url));
            cmd.arg(format!(
                "--window-size={},{}",
                self.config.width, self.config.height
            ));
            cmd.arg("--no-first-run");
            cmd.arg("--no-default-browser-check");

            if let Some(ref prof) = profile_dir {
                cmd.arg(format!("--user-data-dir={}", prof.display()));
            }

            match cmd.spawn() {
                Ok(_) => Ok(DesktopLaunchReport {
                    url,
                    browser_binary: browser,
                    is_app_mode: true,
                    profile_dir,
                }),
                Err(_) => self.fallback_launch(&url),
            }
        } else {
            self.fallback_launch(&url)
        }
    }

    /// Fallback launcher using system default handler (e.g. `xdg-open`)
    fn fallback_launch(&self, url: &str) -> std::io::Result<DesktopLaunchReport> {
        let launcher = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };

        let _ = Command::new(launcher).arg(url).spawn();

        Ok(DesktopLaunchReport {
            url: url.to_string(),
            browser_binary: launcher.to_string(),
            is_app_mode: false,
            profile_dir: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_config_default() {
        let cfg = DesktopLaunchConfig::default();
        assert_eq!(cfg.port, 38472);
        assert_eq!(cfg.width, 1400);
        assert_eq!(cfg.height, 900);
        assert!(cfg.isolated_profile);
    }

    #[test]
    fn test_launcher_instantiation() {
        let launcher = DesktopIdeLauncher::new(None);
        assert_eq!(launcher.config.port, 38472);
    }
}
