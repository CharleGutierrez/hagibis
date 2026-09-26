use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TraceEventKind {
    CommandExecution {
        command: String,
        exit_code: i32,
        duration_ms: u64,
        stdout_tail: String,
        stderr_tail: String,
    },
    ToolCall {
        name: String,
        args_summary: String,
        success: bool,
        duration_ms: u64,
    },
    HttpProbe {
        method: String,
        url: String,
        status_code: u16,
        latency_ms: u64,
    },
    FileMutation {
        path: PathBuf,
        action: String,
        bytes_affected: usize,
    },
    AgentThought {
        thought: String,
    },
    SystemAlert {
        level: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceEvent {
    pub sequence: u64,
    pub timestamp_rfc3339: String,
    pub kind: TraceEventKind,
}

/// Ambient Execution Recorder & Ring Buffer
pub struct TraceRingBuffer {
    capacity: usize,
    events: RwLock<VecDeque<TraceEvent>>,
    counter: AtomicU64,
}

impl Default for TraceRingBuffer {
    fn default() -> Self {
        Self::new(50)
    }
}

impl TraceRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: std::cmp::max(1, capacity),
            events: RwLock::new(VecDeque::with_capacity(capacity)),
            counter: AtomicU64::new(1),
        }
    }

    /// Record a generic trace event
    pub fn record(&self, kind: TraceEventKind) -> u64 {
        let seq = self.counter.fetch_add(1, Ordering::SeqCst);
        let event = TraceEvent {
            sequence: seq,
            timestamp_rfc3339: Utc::now().to_rfc3339(),
            kind,
        };

        if let Ok(mut q) = self.events.write() {
            if q.len() >= self.capacity {
                q.pop_front();
            }
            q.push_back(event);
        }
        seq
    }

    pub fn record_command(
        &self,
        command: &str,
        exit_code: i32,
        duration_ms: u64,
        stdout: &str,
        stderr: &str,
    ) -> u64 {
        let stdout_tail = if stdout.len() > 500 {
            format!("...{}", &stdout[stdout.len() - 500..])
        } else {
            stdout.to_string()
        };
        let stderr_tail = if stderr.len() > 500 {
            format!("...{}", &stderr[stderr.len() - 500..])
        } else {
            stderr.to_string()
        };

        self.record(TraceEventKind::CommandExecution {
            command: command.to_string(),
            exit_code,
            duration_ms,
            stdout_tail,
            stderr_tail,
        })
    }

    pub fn record_tool(&self, name: &str, args_summary: &str, success: bool, duration_ms: u64) -> u64 {
        self.record(TraceEventKind::ToolCall {
            name: name.to_string(),
            args_summary: args_summary.to_string(),
            success,
            duration_ms,
        })
    }

    pub fn record_http(&self, method: &str, url: &str, status_code: u16, latency_ms: u64) -> u64 {
        self.record(TraceEventKind::HttpProbe {
            method: method.to_string(),
            url: url.to_string(),
            status_code,
            latency_ms,
        })
    }

    pub fn record_file_mutation(&self, path: PathBuf, action: &str, bytes_affected: usize) -> u64 {
        self.record(TraceEventKind::FileMutation {
            path,
            action: action.to_string(),
            bytes_affected,
        })
    }

    pub fn record_thought(&self, thought: &str) -> u64 {
        self.record(TraceEventKind::AgentThought {
            thought: thought.to_string(),
        })
    }

    pub fn record_alert(&self, level: &str, message: &str) -> u64 {
        self.record(TraceEventKind::SystemAlert {
            level: level.to_string(),
            message: message.to_string(),
        })
    }

    pub fn record_browser_incident(&self, incident_type: &str, details: &str) -> u64 {
        self.record(TraceEventKind::SystemAlert {
            level: format!("BROWSER_{}", incident_type),
            message: details.to_string(),
        })
    }

    /// Retrieve the most recent N events
    pub fn get_recent(&self, n: usize) -> Vec<TraceEvent> {
        if let Ok(q) = self.events.read() {
            let start = if q.len() > n { q.len() - n } else { 0 };
            q.iter().skip(start).cloned().collect()
        } else {
            vec![]
        }
    }

    pub fn len(&self) -> usize {
        self.events.read().map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut q) = self.events.write() {
            q.clear();
        }
    }

    /// Render XML post-mortem dump optimized for autonomous agent self-healing prompt injection
    pub fn render_post_mortem(&self, last_n: usize) -> String {
        let events = self.get_recent(last_n);
        let total = self.counter.load(Ordering::SeqCst).saturating_sub(1);
        let mut out = String::new();
        out.push_str(&format!(
            "<ambient_execution_recorder total_recorded=\"{}\" showing_recent=\"{}\">\n",
            total,
            events.len()
        ));

        for ev in &events {
            match &ev.kind {
                TraceEventKind::CommandExecution { command, exit_code, duration_ms, stdout_tail, stderr_tail } => {
                    let status = if *exit_code == 0 { "SUCCESS" } else { "FAILED" };
                    out.push_str(&format!(
                        "  <event seq=\"{}\" type=\"Command\" exit_code=\"{}\" status=\"{}\" duration=\"{}ms\">\n",
                        ev.sequence, exit_code, status, duration_ms
                    ));
                    out.push_str(&format!("    <cmd>{}</cmd>\n", escape_xml(command)));
                    if !stdout_tail.is_empty() {
                        out.push_str(&format!("    <stdout>{}</stdout>\n", escape_xml(stdout_tail)));
                    }
                    if !stderr_tail.is_empty() {
                        out.push_str(&format!("    <stderr>{}</stderr>\n", escape_xml(stderr_tail)));
                    }
                    out.push_str("  </event>\n");
                }
                TraceEventKind::ToolCall { name, args_summary, success, duration_ms } => {
                    out.push_str(&format!(
                        "  <event seq=\"{}\" type=\"ToolCall\" tool=\"{}\" success=\"{}\" duration=\"{}ms\">\n",
                        ev.sequence, escape_xml(name), success, duration_ms
                    ));
                    out.push_str(&format!("    <args>{}</args>\n", escape_xml(args_summary)));
                    out.push_str("  </event>\n");
                }
                TraceEventKind::HttpProbe { method, url, status_code, latency_ms } => {
                    out.push_str(&format!(
                        "  <event seq=\"{}\" type=\"HttpProbe\" method=\"{}\" status=\"{}\" latency=\"{}ms\" url=\"{}\" />\n",
                        ev.sequence, method, status_code, latency_ms, escape_xml(url)
                    ));
                }
                TraceEventKind::FileMutation { path, action, bytes_affected } => {
                    out.push_str(&format!(
                        "  <event seq=\"{}\" type=\"FileMutation\" action=\"{}\" bytes=\"{}\" path=\"{}\" />\n",
                        ev.sequence, action, bytes_affected, escape_xml(&path.display().to_string())
                    ));
                }
                TraceEventKind::AgentThought { thought } => {
                    out.push_str(&format!(
                        "  <event seq=\"{}\" type=\"Thought\">{}</event>\n",
                        ev.sequence, escape_xml(thought)
                    ));
                }
                TraceEventKind::SystemAlert { level, message } => {
                    out.push_str(&format!(
                        "  <event seq=\"{}\" type=\"Alert\" level=\"{}\">{}</event>\n",
                        ev.sequence, level, escape_xml(message)
                    ));
                }
            }
        }

        out.push_str("</ambient_execution_recorder>");
        out
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_ring_buffer_bounded() {
        let ring = TraceRingBuffer::new(3);
        ring.record_thought("Thought 1");
        ring.record_thought("Thought 2");
        ring.record_thought("Thought 3");
        ring.record_thought("Thought 4");

        assert_eq!(ring.len(), 3);
        let recent = ring.get_recent(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].sequence, 2);
        assert_eq!(recent[2].sequence, 4);

        let xml = ring.render_post_mortem(2);
        assert!(xml.contains("<ambient_execution_recorder"));
        assert!(xml.contains("type=\"Thought\""));
    }
}
