use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PtyClientMessage {
    #[serde(rename = "input")]
    Input { data: String },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtySessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: u64,
}

pub struct PtyManager {
    workspace_root: PathBuf,
    sessions: Arc<RwLock<HashMap<String, PtySessionMeta>>>,
}

impl PtyManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// List all currently active terminal sessions
    pub async fn list_sessions(&self) -> Vec<PtySessionMeta> {
        let lock = self.sessions.read().await;
        let mut list: Vec<PtySessionMeta> = lock.values().cloned().collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        list
    }

    /// Register a new terminal session
    pub async fn create_session(&self, title: Option<String>) -> PtySessionMeta {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut lock = self.sessions.write().await;
        let next_idx = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let id = format!("term-{}", next_idx);
        let session_title = title.unwrap_or_else(|| format!("Terminal {}", next_idx));

        let meta = PtySessionMeta {
            id: id.clone(),
            title: session_title,
            created_at: now,
        };
        lock.insert(id, meta.clone());
        meta
    }

    /// Remove a terminal session
    pub async fn remove_session(&self, session_id: &str) -> bool {
        let mut lock = self.sessions.write().await;
        lock.remove(session_id).is_some()
    }

    pub async fn handle_websocket(
        self: Arc<Self>,
        socket: WebSocket,
        working_dir: Option<PathBuf>,
        session_id: Option<String>,
    ) {
        let sid = session_id.unwrap_or_else(|| "default".to_string());

        // Ensure session metadata exists
        {
            let mut lock = self.sessions.write().await;
            if !lock.contains_key(&sid) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                lock.insert(
                    sid.clone(),
                    PtySessionMeta {
                        id: sid.clone(),
                        title: sid.clone(),
                        created_at: now,
                    },
                );
            }
        }

        let dir = working_dir.unwrap_or_else(|| self.workspace_root.clone());
        let pty_system = native_pty_system();
        let pair = match pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to open PTY: {}", e))).await;
                self.remove_session(&sid).await;
                return;
            }
        };

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(&dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("HGB_TERMINAL_SESSION", &sid);

        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to spawn shell: {}", e))).await;
                self.remove_session(&sid).await;
                return;
            }
        };

        drop(pair.slave);

        let mut writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to take PTY writer: {}", e))).await;
                self.remove_session(&sid).await;
                return;
            }
        };

        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to clone PTY reader: {}", e))).await;
                self.remove_session(&sid).await;
                return;
            }
        };

        let master_mutex = Arc::new(Mutex::new(pair.master));
        let (mut ws_sender, mut ws_receiver) = socket.split();

        // Background thread to read raw PTY bytes and send over channel
        let (tx_out, mut rx_out) = tokio::sync::mpsc::channel::<Vec<u8>>(128);
        let read_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx_out.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Dedicated background thread for asynchronous non-blocking PTY writes
        let (tx_in, mut rx_in) = tokio::sync::mpsc::channel::<Vec<u8>>(128);
        let write_handle = tokio::task::spawn_blocking(move || {
            while let Some(data) = rx_in.blocking_recv() {
                if writer.write_all(&data).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        // WebSocket transmitter task
        let forward_task = tokio::spawn(async move {
            while let Some(data) = rx_out.recv().await {
                if ws_sender.send(Message::Binary(data)).await.is_err() {
                    break;
                }
            }
        });

        // Incoming keystrokes & resizes from client
        let master_resize = master_mutex.clone();
        while let Some(msg_res) = ws_receiver.next().await {
            match msg_res {
                Ok(Message::Text(text)) => {
                    if let Ok(ctrl) = serde_json::from_str::<PtyClientMessage>(&text) {
                        match ctrl {
                            PtyClientMessage::Input { data } => {
                                if tx_in.send(data.into_bytes()).await.is_err() {
                                    break;
                                }
                            }
                            PtyClientMessage::Resize { cols, rows } => {
                                let m = master_resize.lock().await;
                                let _ = m.resize(PtySize {
                                    rows,
                                    cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                            }
                        }
                    } else {
                        if tx_in.send(text.into_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
                Ok(Message::Binary(bin)) => {
                    if tx_in.send(bin).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => break,
                _ => {}
            }
        }

        let _ = child.kill();
        drop(tx_in);
        forward_task.abort();
        let _ = read_handle.await;
        let _ = write_handle.await;

        // Clean up session from self.sessions upon disconnect
        self.remove_session(&sid).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_client_message_serde() {
        let resize_json = r#"{"type":"resize","cols":120,"rows":35}"#;
        let msg: PtyClientMessage = serde_json::from_str(resize_json).expect("deserialize");
        match msg {
            PtyClientMessage::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 35);
            }
            _ => panic!("wrong variant"),
        }

        let input_json = r#"{"type":"input","data":"ls -la\n"}"#;
        let msg2: PtyClientMessage = serde_json::from_str(input_json).expect("deserialize");
        match msg2 {
            PtyClientMessage::Input { data } => {
                assert_eq!(data, "ls -la\n");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn test_pty_multi_session_management() {
        let mgr = PtyManager::new(PathBuf::from("."));
        let s1 = mgr.create_session(Some("Server".to_string())).await;
        let s2 = mgr.create_session(Some("Worker".to_string())).await;

        let list = mgr.list_sessions().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "Server");
        assert_eq!(list[1].title, "Worker");

        assert!(mgr.remove_session(&s1.id).await);
        let list2 = mgr.list_sessions().await;
        assert_eq!(list2.len(), 1);
        assert_eq!(list2[0].id, s2.id);
    }

    #[tokio::test]
    async fn test_pty_session_id_monotonic_no_collision() {
        let mgr = PtyManager::new(PathBuf::from("."));
        let s1 = mgr.create_session(None).await;
        let s2 = mgr.create_session(None).await;
        assert_ne!(s1.id, s2.id);

        // Delete earlier session to simulate shrinking map
        assert!(mgr.remove_session(&s1.id).await);

        // Creating next session must not collide with s2
        let s3 = mgr.create_session(None).await;
        assert_ne!(s3.id, s2.id);
        assert_ne!(s3.id, s1.id);

        let list = mgr.list_sessions().await;
        assert_eq!(list.len(), 2);
        let ids: Vec<_> = list.into_iter().map(|s| s.id).collect();
        assert!(ids.contains(&s2.id));
        assert!(ids.contains(&s3.id));
    }
}
