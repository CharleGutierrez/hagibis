use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PtyClientMessage {
    #[serde(rename = "input")]
    Input { data: String },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
}

pub struct PtyManager {
    workspace_root: PathBuf,
}

impl PtyManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    pub async fn handle_websocket(self: Arc<Self>, socket: WebSocket, working_dir: Option<PathBuf>) {
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
                return;
            }
        };

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(&dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to spawn shell: {}", e))).await;
                return;
            }
        };

        drop(pair.slave);

        let mut writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to take PTY writer: {}", e))).await;
                return;
            }
        };

        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                let (mut sender, _) = socket.split();
                let _ = sender.send(Message::Text(format!("Failed to clone PTY reader: {}", e))).await;
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
                                let _ = writer.write_all(data.as_bytes());
                                let _ = writer.flush();
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
                        let _ = writer.write_all(text.as_bytes());
                        let _ = writer.flush();
                    }
                }
                Ok(Message::Binary(bin)) => {
                    let _ = writer.write_all(&bin);
                    let _ = writer.flush();
                }
                Ok(Message::Close(_)) => break,
                _ => {}
            }
        }

        let _ = child.kill();
        let _ = read_handle.await;
        forward_task.abort();
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
}
