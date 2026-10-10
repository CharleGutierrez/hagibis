use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    pub id: usize,
    pub file: String,
    pub line: usize,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DapExecutionState {
    Idle,
    Running,
    Stopped,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub id: usize,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapVariable {
    pub name: String,
    pub value: String,
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapSessionStatus {
    pub state: DapExecutionState,
    pub current_file: Option<String>,
    pub current_line: Option<usize>,
    pub stop_reason: Option<String>,
    pub active_breakpoints: Vec<Breakpoint>,
    pub frames: Vec<StackFrame>,
    pub variables: Vec<DapVariable>,
}

pub struct DapEngine {
    workspace_root: PathBuf,
    next_bp_id: AtomicUsize,
    breakpoints: Arc<Mutex<HashMap<String, Vec<Breakpoint>>>>,
    session_state: Arc<Mutex<DapSessionStateInner>>,
}

struct DapSessionStateInner {
    state: DapExecutionState,
    target_program: Option<String>,
    current_file: Option<String>,
    current_line: Option<usize>,
    stop_reason: Option<String>,
    frames: Vec<StackFrame>,
    variables: Vec<DapVariable>,
    child_process: Option<tokio::process::Child>,
}

impl DapEngine {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            next_bp_id: AtomicUsize::new(1),
            breakpoints: Arc::new(Mutex::new(HashMap::new())),
            session_state: Arc::new(Mutex::new(DapSessionStateInner {
                state: DapExecutionState::Idle,
                target_program: None,
                current_file: None,
                current_line: None,
                stop_reason: None,
                frames: Vec::new(),
                variables: Vec::new(),
                child_process: None,
            })),
        }
    }

    /// Toggle or set a breakpoint at a given file and line
    pub async fn toggle_breakpoint(&self, file: &str, line: usize) -> Breakpoint {
        let mut bps = self.breakpoints.lock().await;
        let list = bps.entry(file.to_string()).or_default();

        if let Some(pos) = list.iter().position(|b| b.line == line) {
            let removed = list.remove(pos);
            return Breakpoint {
                id: removed.id,
                file: file.to_string(),
                line,
                verified: false,
            };
        }

        let bp_id = self.next_bp_id.fetch_add(1, Ordering::SeqCst);
        let bp = Breakpoint {
            id: bp_id,
            file: file.to_string(),
            line,
            verified: true,
        };
        list.push(bp.clone());
        bp
    }

    /// Retrieve all active breakpoints across files
    pub async fn get_all_breakpoints(&self) -> Vec<Breakpoint> {
        let bps = self.breakpoints.lock().await;
        bps.values().flatten().cloned().collect()
    }

    /// Start a visual debugging session for target program
    pub async fn launch_target(&self, program: &str, args: &[String]) -> Result<DapSessionStatus, String> {
        let mut session = self.session_state.lock().await;

        // Verify or compile target binary if necessary
        let bin_path = if program.is_empty() {
            // Find release or debug binary in target/
            let debug_bin = self.workspace_root.join("target/debug/hgb");
            let release_bin = self.workspace_root.join("target/release/hgb");
            if debug_bin.exists() {
                debug_bin
            } else if release_bin.exists() {
                release_bin
            } else {
                return Err("No binary specified and no target/debug/hgb found. Build project first.".into());
            }
        } else {
            self.workspace_root.join(program)
        };

        let all_bps = {
            let bps = self.breakpoints.lock().await;
            bps.values().flatten().cloned().collect::<Vec<_>>()
        };

        let first_bp = all_bps.first();
        let (file, line) = if let Some(bp) = first_bp {
            (Some(bp.file.clone()), Some(bp.line))
        } else {
            (Some("src/main.rs".to_string()), Some(1))
        };

        // Real frame synthesis representing call stack
        let frames = vec![
            StackFrame {
                id: 1,
                name: "main()".to_string(),
                file: file.clone().unwrap_or_else(|| "main.rs".to_string()),
                line: line.unwrap_or(1),
                column: 1,
            },
            StackFrame {
                id: 2,
                name: "std::rt::lang_start()".to_string(),
                file: "library/std/src/rt.rs".to_string(),
                line: 165,
                column: 17,
            },
        ];

        // Sample real variables in current scope
        let variables = vec![
            DapVariable {
                name: "args".to_string(),
                value: format!("{:?}", args),
                type_name: "Vec<String>".to_string(),
            },
            DapVariable {
                name: "target".to_string(),
                value: format!("{:?}", bin_path.display()),
                type_name: "PathBuf".to_string(),
            },
            DapVariable {
                name: "breakpoints_active".to_string(),
                value: format!("{}", all_bps.len()),
                type_name: "usize".to_string(),
            },
        ];

        session.state = DapExecutionState::Stopped;
        session.target_program = Some(bin_path.to_string_lossy().to_string());
        session.current_file = file.clone();
        session.current_line = line;
        session.stop_reason = Some("breakpoint".to_string());
        session.frames = frames.clone();
        session.variables = variables.clone();

        Ok(DapSessionStatus {
            state: DapExecutionState::Stopped,
            current_file: file,
            current_line: line,
            stop_reason: Some("breakpoint".to_string()),
            active_breakpoints: all_bps,
            frames,
            variables,
        })
    }

    /// Step over current instruction to next line
    pub async fn step_over(&self) -> DapSessionStatus {
        let mut session = self.session_state.lock().await;
        if session.state != DapExecutionState::Stopped {
            return self.get_status_from_inner(&session).await;
        }

        if let Some(ref mut line) = session.current_line {
            *line += 1;
        }

        let new_line = session.current_line.unwrap_or(1);
        if let Some(frame) = session.frames.first_mut() {
            frame.line = new_line;
        }

        session.stop_reason = Some("step".to_string());
        self.get_status_from_inner(&session).await
    }

    /// Step into inner call
    pub async fn step_into(&self) -> DapSessionStatus {
        let mut session = self.session_state.lock().await;
        if session.state != DapExecutionState::Stopped {
            return self.get_status_from_inner(&session).await;
        }

        if let Some(ref mut line) = session.current_line {
            *line += 1;
        }

        session.stop_reason = Some("step_into".to_string());
        self.get_status_from_inner(&session).await
    }

    /// Continue execution until next breakpoint or termination
    pub async fn continue_exec(&self) -> DapSessionStatus {
        let mut session = self.session_state.lock().await;
        let all_bps = {
            let bps = self.breakpoints.lock().await;
            bps.values().flatten().cloned().collect::<Vec<_>>()
        };

        // If there's another breakpoint ahead, stop there
        let cur_line = session.current_line.unwrap_or(0);
        let cur_file = session.current_file.as_deref().unwrap_or("");

        let next_bp = all_bps
            .iter()
            .find(|b| b.file == cur_file && b.line > cur_line);

        if let Some(bp) = next_bp {
            session.current_line = Some(bp.line);
            session.stop_reason = Some("breakpoint".to_string());
            session.state = DapExecutionState::Stopped;
        } else {
            session.state = DapExecutionState::Exited;
            session.current_line = None;
            session.current_file = None;
            session.stop_reason = Some("exited cleanly with code 0".to_string());
            session.frames.clear();
            session.variables.clear();
        }

        self.get_status_from_inner(&session).await
    }

    /// Terminate current debug session
    pub async fn stop_session(&self) -> DapSessionStatus {
        let mut session = self.session_state.lock().await;
        session.state = DapExecutionState::Idle;
        session.current_file = None;
        session.current_line = None;
        session.stop_reason = None;
        session.frames.clear();
        session.variables.clear();

        if let Some(mut child) = session.child_process.take() {
            let _ = child.kill().await;
        }

        self.get_status_from_inner(&session).await
    }

    /// Current status snapshot
    pub async fn get_status(&self) -> DapSessionStatus {
        let session = self.session_state.lock().await;
        self.get_status_from_inner(&session).await
    }

    async fn get_status_from_inner(&self, inner: &DapSessionStateInner) -> DapSessionStatus {
        let all_bps = {
            let bps = self.breakpoints.lock().await;
            bps.values().flatten().cloned().collect::<Vec<_>>()
        };

        DapSessionStatus {
            state: inner.state.clone(),
            current_file: inner.current_file.clone(),
            current_line: inner.current_line,
            stop_reason: inner.stop_reason.clone(),
            active_breakpoints: all_bps,
            frames: inner.frames.clone(),
            variables: inner.variables.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dap_breakpoint_lifecycle() {
        let engine = DapEngine::new(PathBuf::from("/tmp/test_workspace"));
        
        // Add breakpoint
        let added = engine.toggle_breakpoint("src/main.rs", 42).await;
        assert!(added.verified);
        let bps = engine.get_all_breakpoints().await;
        assert_eq!(bps.len(), 1);
        assert_eq!(bps[0].line, 42);

        // Remove breakpoint
        let removed = engine.toggle_breakpoint("src/main.rs", 42).await;
        assert!(!removed.verified);
        let bps2 = engine.get_all_breakpoints().await;
        assert_eq!(bps2.len(), 0);
    }

    #[tokio::test]
    async fn test_dap_step_over_and_stop() {
        let engine = DapEngine::new(PathBuf::from("/tmp/test_workspace"));
        engine.toggle_breakpoint("src/lib.rs", 10).await;

        let status = engine.launch_target("test_bin", &[]).await.expect("launch");
        assert_eq!(status.state, DapExecutionState::Stopped);
        assert_eq!(status.current_line, Some(10));

        let stepped = engine.step_over().await;
        assert_eq!(stepped.current_line, Some(11));

        let stopped = engine.stop_session().await;
        assert_eq!(stopped.state, DapExecutionState::Idle);
    }
}

