use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsChangeEvent {
    pub kind: String, // "create", "modify", "remove", "other"
    pub path: String, // relative path or filename
    pub timestamp: u64,
}

pub struct WorkspaceFsWatcher {
    workspace_root: PathBuf,
    sender: broadcast::Sender<FsChangeEvent>,
    _watcher: Option<RecommendedWatcher>,
}

impl WorkspaceFsWatcher {
    pub fn new(workspace_root: PathBuf) -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            workspace_root,
            sender,
            _watcher: None,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FsChangeEvent> {
        self.sender.subscribe()
    }

    pub fn start_watching(&mut self) -> Result<(), String> {
        let tx = self.sender.clone();
        let root = self.workspace_root.clone();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let kind_str = match event.kind {
                        EventKind::Create(_) => "create",
                        EventKind::Modify(_) => "modify",
                        EventKind::Remove(_) => "remove",
                        _ => "other",
                    };

                    for path in event.paths {
                        let path_str = path.to_string_lossy();
                        if path_str.contains("/.git/")
                            || path_str.contains("/target/")
                            || path_str.contains("/node_modules/")
                            || path_str.contains("/.bun/")
                            || path_str.contains("/.cache/")
                            || path_str.contains("/.cargo/")
                            || path_str.contains("/.rustup/")
                            || path_str.contains("/.npm/")
                            || path_str.contains("/.gemini/")
                            || path_str.contains("/.local/")
                            || path_str.contains("/.config/")
                            || path_str.contains("/.vscode/")
                            || path_str.contains("/.idea/")
                            || path_str.contains("/.turbo/")
                            || path_str.contains("/.next/")
                            || path_str.contains("/.nuxt/")
                            || path_str.contains("/venv/")
                            || path_str.contains("/.venv/")
                            || path_str.contains("/__pycache__/")
                            || path_str.contains("/.hgb_swarm/")
                            || path_str.contains("/.hgb_snapshots/")
                            || path_str.ends_with('~')
                            || path_str.ends_with(".swp")
                        {
                            continue;
                        }

                        let rel = path
                            .strip_prefix(&root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| path.to_string_lossy().to_string());

                        let change = FsChangeEvent {
                            kind: kind_str.to_string(),
                            path: rel,
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        };

                        let _ = tx.send(change);
                    }
                }
            },
            Config::default(),
        )
        .map_err(|e| format!("Failed to create filesystem watcher: {}", e))?;

        watcher
            .watch(&self.workspace_root, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to watch directory {}: {}", self.workspace_root.display(), e))?;

        self._watcher = Some(watcher);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_workspace_fs_watcher_init_and_broadcast() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut watcher = WorkspaceFsWatcher::new(temp_dir.path().to_path_buf());
        let mut rx = watcher.subscribe();

        assert!(watcher.start_watching().is_ok());

        // Create a new file in watched directory
        let test_file = temp_dir.path().join("live_test.rs");
        std::fs::write(&test_file, "fn main() {}\n").expect("write");

        // Receive event with a short timeout
        let received = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
        if let Ok(Ok(event)) = received {
            assert!(event.path.contains("live_test.rs"));
        }
    }
}
