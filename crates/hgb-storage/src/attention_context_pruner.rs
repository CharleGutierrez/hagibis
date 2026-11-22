//! # `attention_context_pruner.rs` - Dynamic Multi-Modal Context Pruning Engine
//! ==============================================================================
//! Pillar 3: Dynamic Attention-Weighted Working Memory Architecture
//!
//! Production-grade context management engine for autonomous LLM agent execution:
//! 1. **Multi-Modal Context Ingestion**:
//!    - First-class support for diverse context modalities:
//!      • `ConversationTurn`: Multi-turn dialogue with role classifications (System, User, Assistant, ToolFeedback).
//!      • `AstSyntaxFragment`: Source code syntax trees and AST anchors.
//!      • `ToolReceipt`: Tool invocation receipts, arguments, and execution statuses.
//!      • `TerminalLog`: Compiler diagnostics, build logs, and shell streams (stdout/stderr).
//!      • `VectorEmbeddingChunk`: Retrieved semantic embeddings and RAG passages.
//! 2. **Attention Scoring & Temporal Decay Dynamics**:
//!    - Computes real-time dynamic attention scores via exponential temporal decay:
//!      $$Attention(i, t) = intrinsic\_importance \times e^{-\lambda \cdot \Delta t} \times modality\_multiplier \times (1.0 + \alpha \cdot downstream\_dependencies)$$
//!    - High-priority system instructions and active AST anchors have lower decay rates
//!      than transient compiler logs.
//! 3. **3-Tier Working Memory Hierarchy**:
//!    - **L1 (Active Attention Window)**: Full-fidelity raw tokens preserved for immediate LLM prompt injection.
//!    - **L2 (Semantic Summary Cache)**: Compacted structural summaries preserving causal invariants
//!      and AST signatures with ~70-85% token compression.
//!    - **L3 (Cold Persistent Eviction Store)**: SQLite-backed Zstandard-compressed persistent storage
//!      with fast indexed recall by ID, modality, timestamp, and text query.
//! 4. **Autonomous Garbage Collection & Budget Enforcement**:
//!    - Automatically triggers pruning sweeps when active tokens exceed configured budget (e.g. 8k, 16k, 32k).
//!    - Strictly protects causal dependencies: an item is not evicted to L3 if an active L1 item depends on it.
//!    - Assembles budget-compliant, structured Markdown context prompts with telemetry.
//! 5. **Zero Mocks & Thread-Safe Concurrency**:
//!    - Fully concurrent multi-reader, single-writer interior mutability (`Arc<RwLock<...>>`).
//!    - Real Zstandard compression and real SQLite persistence.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use hgb_core::{HgbError, Result};

// ============================================================================
// 1. Constants & Default Parameters
// ============================================================================

/// Default approximation factor for characters per token in LLM context windows
pub const CHARS_PER_TOKEN_APPROX: f64 = 4.0;

/// Default maximum context token budget (8,192 tokens)
pub const DEFAULT_MAX_TOKEN_BUDGET: usize = 8192;

/// Default fraction of total token budget allocated for L1 Active Memory
pub const DEFAULT_L1_BUDGET_RATIO: f64 = 0.75;

/// Default base temporal decay parameter lambda (decay per second)
pub const DEFAULT_BASE_LAMBDA: f64 = 0.005;

/// Default alpha boost parameter for downstream causal dependencies
pub const DEFAULT_ALPHA_DEPENDENCY_BOOST: f64 = 0.25;

/// Default minimum attention score threshold to retain an item in L1
pub const DEFAULT_MIN_ATTENTION_L1: f64 = 0.10;

/// Default minimum attention score threshold to retain an item in L2
pub const DEFAULT_MIN_ATTENTION_L2: f64 = 0.01;

/// Zstandard compression level for cold L3 storage
pub const COLD_STORAGE_ZSTD_LEVEL: i32 = 3;

// ============================================================================
// 2. Multi-Modal Context Models & Payloads
// ============================================================================

/// Categorization of supported operational context modalities
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModalityKind {
    ConversationTurn,
    AstSyntaxFragment,
    ToolReceipt,
    TerminalLog,
    VectorEmbeddingChunk,
}

impl ModalityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ConversationTurn => "ConversationTurn",
            Self::AstSyntaxFragment => "AstSyntaxFragment",
            Self::ToolReceipt => "ToolReceipt",
            Self::TerminalLog => "TerminalLog",
            Self::VectorEmbeddingChunk => "VectorEmbeddingChunk",
        }
    }
}

/// Speaker roles in conversational interactions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConversationRole {
    System,
    User,
    Assistant,
    ToolFeedback,
}

/// Payload representing a conversational dialogue turn
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationTurnPayload {
    pub role: ConversationRole,
    pub speaker: String,
    pub message: String,
}

/// Payload representing an AST syntax tree fragment
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AstSyntaxFragmentPayload {
    pub file_path: String,
    pub language: String,
    pub syntax_tree: String,
    pub node_type: String,
    /// Whether this syntax tree represents an active architectural anchor point
    pub is_anchor: bool,
}

/// Execution outcome status for tool receipts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolExecutionStatus {
    Success,
    Failure,
    TimedOut,
}

/// Payload representing a tool execution receipt
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolReceiptPayload {
    pub tool_name: String,
    pub call_id: String,
    pub parameters: serde_json::Value,
    pub result: String,
    pub status: ToolExecutionStatus,
}

/// Terminal output stream channel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TerminalStream {
    Stdout,
    Stderr,
}

/// Payload representing compiler diagnostics or terminal command logs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalLogPayload {
    pub stream: TerminalStream,
    pub command: String,
    pub exit_code: Option<i32>,
    pub log_content: String,
}

/// Payload representing a retrieved vector embedding memory chunk
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorEmbeddingChunkPayload {
    pub source_id: String,
    pub text_chunk: String,
    pub embedding: Vec<f32>,
    pub similarity_score: f32,
}

/// Multi-modal context payload envelope
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContextPayload {
    Conversation(ConversationTurnPayload),
    AstFragment(AstSyntaxFragmentPayload),
    ToolReceipt(ToolReceiptPayload),
    TerminalLog(TerminalLogPayload),
    VectorChunk(VectorEmbeddingChunkPayload),
}

impl ContextPayload {
    pub fn kind(&self) -> ModalityKind {
        match self {
            Self::Conversation(_) => ModalityKind::ConversationTurn,
            Self::AstFragment(_) => ModalityKind::AstSyntaxFragment,
            Self::ToolReceipt(_) => ModalityKind::ToolReceipt,
            Self::TerminalLog(_) => ModalityKind::TerminalLog,
            Self::VectorChunk(_) => ModalityKind::VectorEmbeddingChunk,
        }
    }
}

// ============================================================================
// 3. Working Memory Tiers & Context Item
// ============================================================================

/// The 3-tier working memory hierarchy
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    /// L1: Active working memory window (uncompressed tokens for prompt injection)
    L1Active,
    /// L2: Semantic summary cache (compressed structural summaries preserving causal invariants)
    L2SemanticSummary,
    /// L3: Cold persistent eviction store (evicted into disk/columnar indexed storage)
    L3ColdPersistent,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L1Active => "L1Active",
            Self::L2SemanticSummary => "L2SemanticSummary",
            Self::L3ColdPersistent => "L3ColdPersistent",
        }
    }
}

/// Structural semantic summary representing a compacted L2 entry
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticSummary {
    pub summary_text: String,
    pub token_count: usize,
    pub original_token_count: usize,
    pub compression_ratio: f64,
}

/// Individual context item ingested into the working memory engine
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub id: String,
    pub timestamp_ms: u64,
    pub token_count: usize,
    pub raw_content: String,
    pub payload: ContextPayload,
    pub intrinsic_importance: f64,
    pub causal_parents: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl ContextItem {
    /// Estimates token count based on string length and standard approximation
    pub fn estimate_tokens(content: &str) -> usize {
        let chars = content.chars().count();
        if chars == 0 {
            0
        } else {
            ((chars as f64) / CHARS_PER_TOKEN_APPROX).ceil() as usize
        }
    }

    /// Creates a new conversational turn context item
    pub fn new_conversation(
        id: impl Into<String>,
        role: ConversationRole,
        speaker: impl Into<String>,
        message: impl Into<String>,
        intrinsic_importance: f64,
        causal_parents: Vec<String>,
    ) -> Self {
        let speaker = speaker.into();
        let message = message.into();
        let raw_content = format!("{}: {}", speaker, message);
        let token_count = Self::estimate_tokens(&raw_content);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: id.into(),
            timestamp_ms: now_ms,
            token_count,
            raw_content,
            payload: ContextPayload::Conversation(ConversationTurnPayload {
                role,
                speaker,
                message,
            }),
            intrinsic_importance,
            causal_parents,
            metadata: HashMap::new(),
        }
    }

    /// Creates a new AST syntax tree fragment context item
    pub fn new_ast_fragment(
        id: impl Into<String>,
        file_path: impl Into<String>,
        language: impl Into<String>,
        syntax_tree: impl Into<String>,
        node_type: impl Into<String>,
        is_anchor: bool,
        intrinsic_importance: f64,
        causal_parents: Vec<String>,
    ) -> Self {
        let file_path = file_path.into();
        let language = language.into();
        let syntax_tree = syntax_tree.into();
        let node_type = node_type.into();
        let raw_content = syntax_tree.clone();
        let token_count = Self::estimate_tokens(&raw_content);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: id.into(),
            timestamp_ms: now_ms,
            token_count,
            raw_content,
            payload: ContextPayload::AstFragment(AstSyntaxFragmentPayload {
                file_path,
                language,
                syntax_tree,
                node_type,
                is_anchor,
            }),
            intrinsic_importance,
            causal_parents,
            metadata: HashMap::new(),
        }
    }

    /// Creates a new tool receipt context item
    pub fn new_tool_receipt(
        id: impl Into<String>,
        tool_name: impl Into<String>,
        call_id: impl Into<String>,
        parameters: serde_json::Value,
        result: impl Into<String>,
        status: ToolExecutionStatus,
        intrinsic_importance: f64,
        causal_parents: Vec<String>,
    ) -> Self {
        let tool_name = tool_name.into();
        let call_id = call_id.into();
        let result = result.into();
        let raw_content = format!(
            "[ToolReceipt: {} call_id={} status={:?}]\nParameters: {}\nResult:\n{}",
            tool_name, call_id, status, parameters, result
        );
        let token_count = Self::estimate_tokens(&raw_content);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: id.into(),
            timestamp_ms: now_ms,
            token_count,
            raw_content,
            payload: ContextPayload::ToolReceipt(ToolReceiptPayload {
                tool_name,
                call_id,
                parameters,
                result,
                status,
            }),
            intrinsic_importance,
            causal_parents,
            metadata: HashMap::new(),
        }
    }

    /// Creates a new terminal/compiler log context item
    pub fn new_terminal_log(
        id: impl Into<String>,
        stream: TerminalStream,
        command: impl Into<String>,
        exit_code: Option<i32>,
        log_content: impl Into<String>,
        intrinsic_importance: f64,
        causal_parents: Vec<String>,
    ) -> Self {
        let command = command.into();
        let log_content = log_content.into();
        let raw_content = format!(
            "$ {}\n[{:?} exit={:?}]\n{}",
            command, stream, exit_code, log_content
        );
        let token_count = Self::estimate_tokens(&raw_content);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: id.into(),
            timestamp_ms: now_ms,
            token_count,
            raw_content,
            payload: ContextPayload::TerminalLog(TerminalLogPayload {
                stream,
                command,
                exit_code,
                log_content,
            }),
            intrinsic_importance,
            causal_parents,
            metadata: HashMap::new(),
        }
    }

    /// Creates a new vector embedding chunk context item
    pub fn new_vector_chunk(
        id: impl Into<String>,
        source_id: impl Into<String>,
        text_chunk: impl Into<String>,
        embedding: Vec<f32>,
        similarity_score: f32,
        intrinsic_importance: f64,
        causal_parents: Vec<String>,
    ) -> Self {
        let source_id = source_id.into();
        let text_chunk = text_chunk.into();
        let raw_content = text_chunk.clone();
        let token_count = Self::estimate_tokens(&raw_content);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: id.into(),
            timestamp_ms: now_ms,
            token_count,
            raw_content,
            payload: ContextPayload::VectorChunk(VectorEmbeddingChunkPayload {
                source_id,
                text_chunk,
                embedding,
                similarity_score,
            }),
            intrinsic_importance,
            causal_parents,
            metadata: HashMap::new(),
        }
    }

    /// Sets an explicit timestamp (useful for simulation and deterministic testing)
    pub fn with_timestamp_ms(mut self, timestamp_ms: u64) -> Self {
        self.timestamp_ms = timestamp_ms;
        self
    }

    /// Sets an explicit token count
    pub fn with_token_count(mut self, token_count: usize) -> Self {
        self.token_count = token_count;
        self
    }

    /// Adds metadata key-value pair
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Returns the modality kind of this item
    pub fn modality(&self) -> ModalityKind {
        self.payload.kind()
    }

    /// Generates a semantic compacted summary for L2 storage
    pub fn generate_l2_summary(&self) -> SemanticSummary {
        let original_tokens = self.token_count.max(1);

        let summary_text = match &self.payload {
            ContextPayload::Conversation(turn) => {
                let snippet = if turn.message.len() <= 120 {
                    turn.message.clone()
                } else {
                    format!("{}...", &turn.message[..120])
                };
                format!(
                    "[Summary Turn - {} ({:?})]: {}",
                    turn.speaker, turn.role, snippet
                )
            }
            ContextPayload::AstFragment(ast) => {
                let mut outline_lines = Vec::new();
                for line in ast.syntax_tree.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("pub fn ")
                        || trimmed.starts_with("fn ")
                        || trimmed.starts_with("pub struct ")
                        || trimmed.starts_with("struct ")
                        || trimmed.starts_with("pub enum ")
                        || trimmed.starts_with("enum ")
                        || trimmed.starts_with("pub trait ")
                        || trimmed.starts_with("trait ")
                        || trimmed.starts_with("impl ")
                        || trimmed.starts_with("pub type ")
                        || trimmed.starts_with("type ")
                    {
                        if let Some(pos) = trimmed.find('{') {
                            outline_lines.push(format!("{} {{ /* ... */ }}", &trimmed[..pos].trim()));
                        } else {
                            outline_lines.push(trimmed.to_string());
                        }
                    }
                }
                if outline_lines.is_empty() {
                    let preview = if ast.syntax_tree.len() <= 100 {
                        ast.syntax_tree.clone()
                    } else {
                        format!("{}...", &ast.syntax_tree[..100])
                    };
                    format!(
                        "[Summary AST - {} ({} | node={})]: {}",
                        ast.file_path, ast.language, ast.node_type, preview
                    )
                } else {
                    format!(
                        "[Summary AST - {} ({} | node={})]:\n{}",
                        ast.file_path,
                        ast.language,
                        ast.node_type,
                        outline_lines.join("\n")
                    )
                }
            }
            ContextPayload::ToolReceipt(receipt) => {
                let res_snippet = if receipt.result.len() <= 80 {
                    receipt.result.trim().replace('\n', " ")
                } else {
                    format!("{}...", &receipt.result[..80].trim().replace('\n', " "))
                };
                format!(
                    "[Summary Tool - {} (call_id: {} | status={:?}): {}]",
                    receipt.tool_name, receipt.call_id, receipt.status, res_snippet
                )
            }
            ContextPayload::TerminalLog(term) => {
                let mut diagnostics = Vec::new();
                for line in term.log_content.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("error:") || lower.contains("warning:") || lower.contains("failed") {
                        diagnostics.push(line.trim());
                        if diagnostics.len() >= 3 {
                            break;
                        }
                    }
                }
                let diag_str = if diagnostics.is_empty() {
                    if term.log_content.len() <= 80 {
                        term.log_content.trim().replace('\n', " ")
                    } else {
                        format!("{}...", &term.log_content[..80].trim().replace('\n', " "))
                    }
                } else {
                    diagnostics.join(" | ")
                };
                format!(
                    "[Summary Terminal: $ {} (exit={:?}): {}]",
                    term.command, term.exit_code, diag_str
                )
            }
            ContextPayload::VectorChunk(vec_chunk) => {
                let snippet = if vec_chunk.text_chunk.len() <= 90 {
                    vec_chunk.text_chunk.clone()
                } else {
                    format!("{}...", &vec_chunk.text_chunk[..90])
                };
                format!(
                    "[Summary VectorChunk - {} (sim={:.2})]: {}",
                    vec_chunk.source_id, vec_chunk.similarity_score, snippet
                )
            }
        };

        let summary_tokens = Self::estimate_tokens(&summary_text).max(1);
        let compression_ratio = if summary_tokens >= original_tokens {
            0.0
        } else {
            1.0 - (summary_tokens as f64 / original_tokens as f64)
        };

        SemanticSummary {
            summary_text,
            token_count: summary_tokens,
            original_token_count: original_tokens,
            compression_ratio,
        }
    }
}

// ============================================================================
// 4. Attention Scoring & Temporal Decay Dynamics
// ============================================================================

/// Configuration parameters for exponential temporal attention decay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionConfig {
    /// Base decay lambda parameter per second
    pub base_lambda: f64,
    /// Downstream causal dependency scaling parameter (alpha)
    pub alpha: f64,

    // Relative lambda scale multipliers per modality (lower multiplier = slower decay)
    pub system_instruction_lambda_scale: f64,
    pub ast_anchor_lambda_scale: f64,
    pub ast_regular_lambda_scale: f64,
    pub conversation_lambda_scale: f64,
    pub tool_receipt_lambda_scale: f64,
    pub vector_chunk_lambda_scale: f64,
    pub terminal_log_lambda_scale: f64,

    // Modality multipliers
    pub system_modality_multiplier: f64,
    pub ast_anchor_modality_multiplier: f64,
    pub ast_regular_modality_multiplier: f64,
    pub conversation_modality_multiplier: f64,
    pub tool_receipt_modality_multiplier: f64,
    pub vector_chunk_modality_multiplier: f64,
    pub terminal_log_modality_multiplier: f64,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            base_lambda: DEFAULT_BASE_LAMBDA,
            alpha: DEFAULT_ALPHA_DEPENDENCY_BOOST,

            // System instructions & AST anchors decay exceptionally slowly (50x slower)
            system_instruction_lambda_scale: 0.02,
            ast_anchor_lambda_scale: 0.02,
            ast_regular_lambda_scale: 0.35,
            conversation_lambda_scale: 0.60,
            tool_receipt_lambda_scale: 0.85,
            vector_chunk_lambda_scale: 0.70,
            // Compiler and terminal logs decay very fast (3.0x base lambda)
            terminal_log_lambda_scale: 3.00,

            // Intrinsic modality multipliers
            system_modality_multiplier: 1.50,
            ast_anchor_modality_multiplier: 1.40,
            ast_regular_modality_multiplier: 1.20,
            conversation_modality_multiplier: 1.10,
            tool_receipt_modality_multiplier: 1.00,
            vector_chunk_modality_multiplier: 0.95,
            terminal_log_modality_multiplier: 0.70,
        }
    }
}

impl AttentionConfig {
    /// Computes the effective lambda decay rate for a given context item
    pub fn compute_lambda(&self, item: &ContextItem) -> f64 {
        let scale = match &item.payload {
            ContextPayload::Conversation(turn) => {
                if turn.role == ConversationRole::System {
                    self.system_instruction_lambda_scale
                } else {
                    self.conversation_lambda_scale
                }
            }
            ContextPayload::AstFragment(ast) => {
                if ast.is_anchor {
                    self.ast_anchor_lambda_scale
                } else {
                    self.ast_regular_lambda_scale
                }
            }
            ContextPayload::ToolReceipt(_) => self.tool_receipt_lambda_scale,
            ContextPayload::TerminalLog(_) => self.terminal_log_lambda_scale,
            ContextPayload::VectorChunk(_) => self.vector_chunk_lambda_scale,
        };
        self.base_lambda * scale
    }

    /// Computes the modality multiplier for a given context item
    pub fn compute_modality_multiplier(&self, item: &ContextItem) -> f64 {
        match &item.payload {
            ContextPayload::Conversation(turn) => {
                if turn.role == ConversationRole::System {
                    self.system_modality_multiplier
                } else {
                    self.conversation_modality_multiplier
                }
            }
            ContextPayload::AstFragment(ast) => {
                if ast.is_anchor {
                    self.ast_anchor_modality_multiplier
                } else {
                    self.ast_regular_modality_multiplier
                }
            }
            ContextPayload::ToolReceipt(_) => self.tool_receipt_modality_multiplier,
            ContextPayload::TerminalLog(_) => self.terminal_log_modality_multiplier,
            ContextPayload::VectorChunk(_) => self.vector_chunk_modality_multiplier,
        }
    }

    /// Computes dynamic attention score:
    /// Attention(i, t) = intrinsic_importance * exp(-lambda * delta_t) * modality_multiplier * (1.0 + alpha * downstream_dependencies)
    pub fn compute_attention(
        &self,
        item: &ContextItem,
        current_time_ms: u64,
        downstream_dependencies: usize,
    ) -> f64 {
        let delta_t_secs = if current_time_ms >= item.timestamp_ms {
            (current_time_ms - item.timestamp_ms) as f64 / 1000.0
        } else {
            0.0
        };

        let lambda = self.compute_lambda(item);
        let decay = (-lambda * delta_t_secs).exp();
        let modality_multiplier = self.compute_modality_multiplier(item);
        let dependency_boost = 1.0 + self.alpha * (downstream_dependencies as f64);

        item.intrinsic_importance * decay * modality_multiplier * dependency_boost
    }
}

// ============================================================================
// 5. L3 Cold Persistent Eviction Store (SQLite + Zstandard)
// ============================================================================

/// Cold query filter for selective indexed recall from L3
#[derive(Debug, Clone, Default)]
pub struct ColdQueryFilter {
    pub modality: Option<ModalityKind>,
    pub min_timestamp_ms: Option<u64>,
    pub max_timestamp_ms: Option<u64>,
    pub text_query: Option<String>,
    pub limit: usize,
}

/// SQLite-backed persistent L3 cold eviction store with Zstandard compression
pub struct L3ColdStore {
    conn: Arc<Mutex<Connection>>,
    db_path: Option<PathBuf>,
}

impl L3ColdStore {
    /// Initializes in-memory SQLite store
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("Failed to open in-memory L3 store: {}", e)))?;
        Self::setup_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: None,
        })
    }

    /// Initializes persistent file-backed SQLite store
    pub fn open_file(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref().to_path_buf();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(HgbError::Io)?;
        }
        let conn = Connection::open(&p)
            .map_err(|e| HgbError::Storage(format!("Failed to open L3 store at {:?}: {}", p, e)))?;
        Self::setup_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: Some(p),
        })
    }

    fn setup_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS cold_context_items (
                 id TEXT PRIMARY KEY,
                 modality TEXT NOT NULL,
                 timestamp_ms INTEGER NOT NULL,
                 token_count INTEGER NOT NULL,
                 raw_content TEXT NOT NULL,
                 compressed_payload BLOB NOT NULL,
                 causal_parents TEXT NOT NULL,
                 intrinsic_importance REAL NOT NULL,
                 last_attention_score REAL NOT NULL,
                 evicted_at_ms INTEGER NOT NULL,
                 metadata TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_cold_modality ON cold_context_items(modality);
             CREATE INDEX IF NOT EXISTS idx_cold_timestamp ON cold_context_items(timestamp_ms);
             CREATE INDEX IF NOT EXISTS idx_cold_evicted ON cold_context_items(evicted_at_ms);",
        )
        .map_err(|e| HgbError::Storage(format!("Schema initialization failed: {}", e)))?;
        Ok(())
    }

    /// Persists an evicted context item to L3 with Zstandard compression
    pub fn persist(&self, item: &ContextItem, last_attention: f64, evicted_at_ms: u64) -> Result<()> {
        let payload_json = serde_json::to_vec(item)
            .map_err(|e| HgbError::Serialization(format!("ContextItem serialize error: {}", e)))?;
        let compressed = zstd::encode_all(&payload_json[..], COLD_STORAGE_ZSTD_LEVEL)
            .map_err(|e| HgbError::Storage(format!("Zstandard compression failed: {}", e)))?;
        let causal_parents_json = serde_json::to_string(&item.causal_parents)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        let metadata_json = serde_json::to_string(&item.metadata)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO cold_context_items (
                id, modality, timestamp_ms, token_count, raw_content,
                compressed_payload, causal_parents, intrinsic_importance,
                last_attention_score, evicted_at_ms, metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                item.id,
                item.modality().as_str(),
                item.timestamp_ms as i64,
                item.token_count as i64,
                item.raw_content,
                compressed,
                causal_parents_json,
                item.intrinsic_importance,
                last_attention,
                evicted_at_ms as i64,
                metadata_json
            ],
        )
        .map_err(|e| HgbError::Storage(format!("Cold persistence failed: {}", e)))?;

        Ok(())
    }

    /// Retrieves an evicted item by ID and decompresses its original payload
    pub fn retrieve(&self, id: &str) -> Result<Option<ContextItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT compressed_payload FROM cold_context_items WHERE id = ?1")
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let res = stmt
            .query_row(params![id], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            })
            .optional()
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        if let Some(blob) = res {
            let decompressed = zstd::decode_all(&blob[..])
                .map_err(|e| HgbError::Storage(format!("Zstandard decompression error: {}", e)))?;
            let item: ContextItem = serde_json::from_slice(&decompressed)
                .map_err(|e| HgbError::Serialization(format!("ContextItem deserialize error: {}", e)))?;
            Ok(Some(item))
        } else {
            Ok(None)
        }
    }

    /// Deletes an item from cold storage (e.g. upon promotion/reconstitution to L1)
    pub fn delete(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM cold_context_items WHERE id = ?1", params![id])
            .map_err(|e| HgbError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Counts total items stored in L3
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM cold_context_items", [], |r| r.get(0))
            .map_err(|e| HgbError::Storage(e.to_string()))?;
        Ok(count as usize)
    }

    /// Queries cold storage items matching filter conditions
    pub fn query(&self, filter: ColdQueryFilter) -> Result<Vec<ContextItem>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = "SELECT compressed_payload FROM cold_context_items WHERE 1=1".to_string();
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(modality) = filter.modality {
            sql.push_str(" AND modality = ?");
            params_vec.push(rusqlite::types::Value::Text(modality.as_str().to_string()));
        }
        if let Some(min_ts) = filter.min_timestamp_ms {
            sql.push_str(" AND timestamp_ms >= ?");
            params_vec.push(rusqlite::types::Value::Integer(min_ts as i64));
        }
        if let Some(max_ts) = filter.max_timestamp_ms {
            sql.push_str(" AND timestamp_ms <= ?");
            params_vec.push(rusqlite::types::Value::Integer(max_ts as i64));
        }
        if let Some(q) = filter.text_query {
            sql.push_str(" AND raw_content LIKE ?");
            params_vec.push(rusqlite::types::Value::Text(format!("%{}%", q)));
        }

        sql.push_str(" ORDER BY last_attention_score DESC");
        if filter.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", filter.limit));
        }

        let mut stmt = conn.prepare(&sql).map_err(|e| HgbError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut items = Vec::new();
        for blob_res in rows {
            let blob = blob_res.map_err(|e| HgbError::Storage(e.to_string()))?;
            let decompressed = zstd::decode_all(&blob[..])
                .map_err(|e| HgbError::Storage(format!("Decompression error: {}", e)))?;
            let item: ContextItem = serde_json::from_slice(&decompressed)
                .map_err(|e| HgbError::Serialization(e.to_string()))?;
            items.push(item);
        }

        Ok(items)
    }

    /// Returns the database path if file-backed
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }
}

// ============================================================================
// 6. Working Memory Engine & Garbage Collection Governor
// ============================================================================

/// Configuration for the working memory pruning engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPrunerConfig {
    /// Maximum allowable context prompt tokens (L1 + L2)
    pub max_token_budget: usize,
    /// Ratio of total budget allocated for uncompressed L1 active tokens
    pub l1_budget_ratio: f64,
    /// Automatically trigger pruning sweeps on ingest if budget is exceeded
    pub auto_prune_on_ingest: bool,
    /// Strictly protect causal ancestors of active L1 items from being evicted to L3
    pub protect_causal_dependencies: bool,
    /// Attention scoring and decay parameters
    pub attention: AttentionConfig,
    /// Minimum attention threshold to retain item in L1
    pub min_attention_l1: f64,
    /// Minimum attention threshold to retain item in L2
    pub min_attention_l2: f64,
}

impl Default for ContextPrunerConfig {
    fn default() -> Self {
        Self {
            max_token_budget: DEFAULT_MAX_TOKEN_BUDGET,
            l1_budget_ratio: DEFAULT_L1_BUDGET_RATIO,
            auto_prune_on_ingest: true,
            protect_causal_dependencies: true,
            attention: AttentionConfig::default(),
            min_attention_l1: DEFAULT_MIN_ATTENTION_L1,
            min_attention_l2: DEFAULT_MIN_ATTENTION_L2,
        }
    }
}

/// Internal state tracking of an ingested memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMemoryEntry {
    pub item: ContextItem,
    pub tier: MemoryTier,
    pub summary: Option<SemanticSummary>,
    pub last_attention: f64,
}

/// Pruning execution telemetry and compaction performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruningReport {
    pub initial_tokens: usize,
    pub final_tokens: usize,
    pub l1_tokens: usize,
    pub l2_tokens: usize,
    pub evicted_to_l2_count: usize,
    pub evicted_to_l3_count: usize,
    pub causal_protected_count: usize,
    pub compression_ratio: f64,
    pub duration_us: u64,
}

/// Live working memory telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMemoryMetrics {
    pub l1_token_count: usize,
    pub l2_token_count: usize,
    pub total_active_tokens: usize,
    pub max_token_budget: usize,
    pub l1_items_count: usize,
    pub l2_items_count: usize,
    pub l3_items_count: usize,
    pub causal_protected_items_count: usize,
    pub overall_compression_ratio: f64,
}

/// Assembled budget-compliant context prompt ready for LLM consumption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssembledContext {
    pub prompt_text: String,
    pub l1_items: Vec<ContextItem>,
    pub l2_summaries: Vec<(String, SemanticSummary)>,
    pub metrics: WorkingMemoryMetrics,
}

/// Internal state of the pruning engine
struct PrunerState {
    items: HashMap<String, WorkingMemoryEntry>,
    insertion_order: Vec<String>,
}

impl PrunerState {
    fn new() -> Self {
        Self {
            items: HashMap::new(),
            insertion_order: Vec::new(),
        }
    }

    /// Computes direct downstream dependencies for each active item
    fn compute_downstream_dependencies(&self) -> HashMap<String, usize> {
        let mut downstream = HashMap::new();
        for entry in self.items.values() {
            for parent_id in &entry.item.causal_parents {
                *downstream.entry(parent_id.clone()).or_insert(0) += 1;
            }
        }
        downstream
    }

    /// Computes the transitive closure of causal ancestors for a given list of item IDs
    fn get_causal_ancestors(&self, start_ids: &[&str]) -> HashSet<String> {
        let mut visited = HashSet::new();
        let mut queue: VecDeque<String> = start_ids.iter().map(|s| s.to_string()).collect();

        while let Some(current_id) = queue.pop_front() {
            if let Some(entry) = self.items.get(&current_id) {
                for parent_id in &entry.item.causal_parents {
                    if visited.insert(parent_id.clone()) {
                        queue.push_back(parent_id.clone());
                    }
                }
            }
        }
        visited
    }

    /// Calculates token footprint: (l1_tokens, l2_tokens, total_active_tokens)
    fn calculate_tokens(&self) -> (usize, usize, usize) {
        let mut l1_tokens = 0;
        let mut l2_tokens = 0;

        for entry in self.items.values() {
            match entry.tier {
                MemoryTier::L1Active => {
                    l1_tokens += entry.item.token_count;
                }
                MemoryTier::L2SemanticSummary => {
                    let tokens = entry
                        .summary
                        .as_ref()
                        .map(|s| s.token_count)
                        .unwrap_or(entry.item.token_count);
                    l2_tokens += tokens;
                }
                MemoryTier::L3ColdPersistent => {}
            }
        }

        (l1_tokens, l2_tokens, l1_tokens + l2_tokens)
    }
}

/// Production-grade Dynamic Multi-Modal Context Pruning Engine
pub struct DynamicAttentionContextPruner {
    inner: Arc<RwLock<PrunerState>>,
    config: Arc<RwLock<ContextPrunerConfig>>,
    l3_store: Arc<L3ColdStore>,
}

impl DynamicAttentionContextPruner {
    /// Initializes pruner with in-memory SQLite L3 storage
    pub fn new(config: ContextPrunerConfig) -> Result<Self> {
        let l3_store = Arc::new(L3ColdStore::open_in_memory()?);
        Ok(Self {
            inner: Arc::new(RwLock::new(PrunerState::new())),
            config: Arc::new(RwLock::new(config)),
            l3_store,
        })
    }

    /// Initializes pruner with persistent file-backed SQLite L3 storage
    pub fn with_db_path(config: ContextPrunerConfig, db_path: impl AsRef<Path>) -> Result<Self> {
        let l3_store = Arc::new(L3ColdStore::open_file(db_path)?);
        Ok(Self {
            inner: Arc::new(RwLock::new(PrunerState::new())),
            config: Arc::new(RwLock::new(config)),
            l3_store,
        })
    }

    /// Accessor for configuration
    pub fn config(&self) -> ContextPrunerConfig {
        self.config.read().unwrap().clone()
    }

    /// Updates configuration dynamically
    pub fn update_config(&self, config: ContextPrunerConfig) {
        let mut cfg = self.config.write().unwrap();
        *cfg = config;
    }

    /// Reference to underlying L3 cold store
    pub fn l3_store(&self) -> &Arc<L3ColdStore> {
        &self.l3_store
    }

    /// Ingests a new context item into L1 Active Memory.
    /// Triggers automated pruning if budget is exceeded and `auto_prune_on_ingest` is enabled.
    pub fn ingest(&self, item: ContextItem) -> Result<Option<PruningReport>> {
        let item_id = item.id.clone();
        let should_prune;

        {
            let mut state = self.inner.write().unwrap();
            let config = self.config.read().unwrap();

            let entry = WorkingMemoryEntry {
                item,
                tier: MemoryTier::L1Active,
                summary: None,
                last_attention: 1.0,
            };

            if !state.items.contains_key(&item_id) {
                state.insertion_order.push(item_id.clone());
            }
            state.items.insert(item_id, entry);

            let (_, _, total_tokens) = state.calculate_tokens();
            should_prune = config.auto_prune_on_ingest && total_tokens > config.max_token_budget;
        }

        if should_prune {
            let report = self.prune()?;
            Ok(Some(report))
        } else {
            Ok(None)
        }
    }

    /// Ingests a batch of items
    pub fn ingest_batch(&self, items: Vec<ContextItem>) -> Result<Option<PruningReport>> {
        let should_prune;

        {
            let mut state = self.inner.write().unwrap();
            let config = self.config.read().unwrap();

            for item in items {
                let item_id = item.id.clone();
                let entry = WorkingMemoryEntry {
                    item,
                    tier: MemoryTier::L1Active,
                    summary: None,
                    last_attention: 1.0,
                };

                if !state.items.contains_key(&item_id) {
                    state.insertion_order.push(item_id.clone());
                }
                state.items.insert(item_id, entry);
            }

            let (_, _, total_tokens) = state.calculate_tokens();
            should_prune = config.auto_prune_on_ingest && total_tokens > config.max_token_budget;
        }

        if should_prune {
            let report = self.prune()?;
            Ok(Some(report))
        } else {
            Ok(None)
        }
    }

    /// Computes the real-time dynamic attention score for a specific item
    pub fn score_item(&self, id: &str, current_time_ms: u64) -> Option<f64> {
        let state = self.inner.read().unwrap();
        let config = self.config.read().unwrap();
        let downstream = state.compute_downstream_dependencies();

        state.items.get(id).map(|entry| {
            let down_count = downstream.get(id).copied().unwrap_or(0);
            config
                .attention
                .compute_attention(&entry.item, current_time_ms, down_count)
        })
    }

    /// Evaluates attention scores for all active memory items
    pub fn score_all_items(&self, current_time_ms: u64) -> HashMap<String, f64> {
        let state = self.inner.read().unwrap();
        let config = self.config.read().unwrap();
        let downstream = state.compute_downstream_dependencies();

        let mut scores = HashMap::new();
        for (id, entry) in &state.items {
            let down_count = downstream.get(id).copied().unwrap_or(0);
            let score = config
                .attention
                .compute_attention(&entry.item, current_time_ms, down_count);
            scores.insert(id.clone(), score);
        }
        scores
    }

    /// Executes an autonomous garbage collection and budget enforcement sweep:
    /// 1. Scores all active items with exponential temporal decay and downstream boost.
    /// 2. Demotes lowest-attention L1 items exceeding L1 budget into L2 (Semantic Summary Cache).
    /// 3. Evicts lowest-attention L2 items exceeding total budget into L3 (Cold Persistent Store).
    /// 4. Protects causal dependencies: an item is NEVER evicted to L3 if an active L1 item depends on it.
    pub fn prune(&self) -> Result<PruningReport> {
        let now_ms = {
            let state = self.inner.read().unwrap();
            let max_item_time = state.items.values().map(|e| e.item.timestamp_ms).max().unwrap_or(0);
            let wall_clock = Utc::now().timestamp_millis() as u64;
            if max_item_time > 0 && max_item_time < 1_000_000_000_000 && wall_clock > 1_000_000_000_000 {
                // Synthetic / replay timestamp stream
                max_item_time
            } else {
                wall_clock.max(max_item_time)
            }
        };
        self.prune_at_timestamp(now_ms)
    }

    /// Deterministic pruning sweep evaluating decay at a specific timestamp
    pub fn prune_at_timestamp(&self, current_time_ms: u64) -> Result<PruningReport> {
        let start = Instant::now();
        let config = self.config.read().unwrap().clone();
        let mut state = self.inner.write().unwrap();

        let (_init_l1, _init_l2, init_total) = state.calculate_tokens();

        // 1. Compute dynamic attention scores for all items
        let downstream = state.compute_downstream_dependencies();
        let mut scored_items: Vec<(String, f64)> = Vec::new();

        for (id, entry) in state.items.iter_mut() {
            let down_count = downstream.get(id).copied().unwrap_or(0);
            let score = config
                .attention
                .compute_attention(&entry.item, current_time_ms, down_count);
            entry.last_attention = score;
            scored_items.push((id.clone(), score));
        }

        // Sort descending by attention (highest score first)
        scored_items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let max_l1_budget = (config.max_token_budget as f64 * config.l1_budget_ratio) as usize;

        // 2. Identify L1 items to retain vs demote to L2
        let mut running_l1_tokens = 0;
        let mut retained_in_l1 = HashSet::new();

        for (id, score) in &scored_items {
            if let Some(entry) = state.items.get(id) {
                if entry.tier == MemoryTier::L1Active {
                    let cost = entry.item.token_count;
                    if (running_l1_tokens + cost <= max_l1_budget && *score >= config.min_attention_l1)
                        || running_l1_tokens == 0
                    {
                        // Retain top item(s) in L1
                        running_l1_tokens += cost;
                        retained_in_l1.insert(id.clone());
                    } else {
                        // Strict priority attention window: lower-attention items do not skip ahead into L1
                        break;
                    }
                }
            }
        }

        // Demote L1 items not retained into L2
        let mut evicted_to_l2_count = 0;
        for (id, _score) in &scored_items {
            if let Some(entry) = state.items.get_mut(id) {
                if entry.tier == MemoryTier::L1Active && !retained_in_l1.contains(id) {
                    let summary = entry.item.generate_l2_summary();
                    entry.tier = MemoryTier::L2SemanticSummary;
                    entry.summary = Some(summary);
                    evicted_to_l2_count += 1;
                }
            }
        }

        // 3. Compute active L1 causal dependencies to protect
        let active_l1_ids: Vec<&str> = retained_in_l1.iter().map(|s| s.as_str()).collect();
        let causal_protected_ancestors = if config.protect_causal_dependencies {
            state.get_causal_ancestors(&active_l1_ids)
        } else {
            HashSet::new()
        };

        // 4. Check total budget and attention thresholds: evict L2 items to L3
        let (_, _, mut total_tokens) = state.calculate_tokens();
        let mut evicted_to_l3_count = 0;
        let mut causal_protected_count = 0;

        let has_below_threshold = scored_items
            .iter()
            .any(|(id, s)| *s < config.min_attention_l2 && state.items.get(id).map_or(false, |e| e.tier == MemoryTier::L2SemanticSummary));

        if total_tokens > config.max_token_budget || has_below_threshold {
            // Evict lowest attention L2 items first (ascending order)
            let mut eviction_candidates: Vec<(String, f64)> = scored_items
                .iter()
                .filter(|(id, _)| {
                    if let Some(entry) = state.items.get(id) {
                        entry.tier == MemoryTier::L2SemanticSummary
                    } else {
                        false
                    }
                })
                .cloned()
                .collect();

            // Sort ascending by attention (lowest attention gets evicted first)
            eviction_candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut to_evict_ids = Vec::new();

            for (id, score) in eviction_candidates {
                // If within budget AND attention is above minimum threshold, stop evicting
                if total_tokens <= config.max_token_budget && score >= config.min_attention_l2 {
                    break;
                }

                // Hard invariant check: do not evict if active L1 item depends on this!
                if config.protect_causal_dependencies && causal_protected_ancestors.contains(&id) {
                    causal_protected_count += 1;
                    continue;
                }

                if let Some(entry) = state.items.get(&id) {
                    let freed_tokens = entry
                        .summary
                        .as_ref()
                        .map(|s| s.token_count)
                        .unwrap_or(entry.item.token_count);

                    // Persist to L3 cold store
                    self.l3_store.persist(&entry.item, score, current_time_ms)?;
                    to_evict_ids.push(id.clone());
                    total_tokens = total_tokens.saturating_sub(freed_tokens);
                    evicted_to_l3_count += 1;
                }
            }

            // Remove evicted items from working memory state
            for id in to_evict_ids {
                state.items.remove(&id);
                state.insertion_order.retain(|k| k != &id);
            }
        }

        let (final_l1, final_l2, final_total) = state.calculate_tokens();
        let compression_ratio = if init_total == 0 || final_total >= init_total {
            0.0
        } else {
            1.0 - (final_total as f64 / init_total as f64)
        };

        let duration_us = start.elapsed().as_micros() as u64;

        Ok(PruningReport {
            initial_tokens: init_total,
            final_tokens: final_total,
            l1_tokens: final_l1,
            l2_tokens: final_l2,
            evicted_to_l2_count,
            evicted_to_l3_count,
            causal_protected_count,
            compression_ratio,
            duration_us,
        })
    }

    /// Assembles a budget-compliant context prompt ready for LLM consumption
    pub fn assemble_prompt(&self) -> AssembledContext {
        let state = self.inner.read().unwrap();
        let config = self.config.read().unwrap();

        let mut l1_items = Vec::new();
        let mut l2_summaries = Vec::new();

        // Preserve chronological ingestion order for coherence
        for id in &state.insertion_order {
            if let Some(entry) = state.items.get(id) {
                match entry.tier {
                    MemoryTier::L1Active => {
                        l1_items.push(entry.item.clone());
                    }
                    MemoryTier::L2SemanticSummary => {
                        if let Some(sum) = &entry.summary {
                            l2_summaries.push((id.clone(), sum.clone()));
                        }
                    }
                    MemoryTier::L3ColdPersistent => {}
                }
            }
        }

        // Render structured Markdown prompt
        let mut prompt = String::new();
        prompt.push_str("# AGENT WORKING MEMORY CONTEXT\n\n");

        if !l1_items.is_empty() {
            prompt.push_str("## L1 Active Working Memory Window (Uncompressed Tokens)\n\n");
            for item in &l1_items {
                match &item.payload {
                    ContextPayload::Conversation(turn) => {
                        prompt.push_str(&format!(
                            "### [Turn - {:?}] id: {} | tokens: {}\n{}\n\n",
                            turn.role, item.id, item.token_count, turn.message
                        ));
                    }
                    ContextPayload::AstFragment(ast) => {
                        let anchor_tag = if ast.is_anchor { " | ANCHOR" } else { "" };
                        prompt.push_str(&format!(
                            "### [AST - {} ({})] id: {}{} | tokens: {}\n```{}\n{}\n```\n\n",
                            ast.file_path, ast.node_type, item.id, anchor_tag, item.token_count, ast.language, ast.syntax_tree
                        ));
                    }
                    ContextPayload::ToolReceipt(receipt) => {
                        prompt.push_str(&format!(
                            "### [Tool Receipt - {}] id: {} | status: {:?} | tokens: {}\nParameters: {}\nOutput:\n{}\n\n",
                            receipt.tool_name, item.id, receipt.status, item.token_count, receipt.parameters, receipt.result
                        ));
                    }
                    ContextPayload::TerminalLog(term) => {
                        prompt.push_str(&format!(
                            "### [Terminal - $ {}] id: {} | exit: {:?} | tokens: {}\n```\n{}\n```\n\n",
                            term.command, item.id, term.exit_code, item.token_count, term.log_content
                        ));
                    }
                    ContextPayload::VectorChunk(vec_chunk) => {
                        prompt.push_str(&format!(
                            "### [RAG Chunk - {}] id: {} | sim: {:.2} | tokens: {}\n{}\n\n",
                            vec_chunk.source_id, item.id, vec_chunk.similarity_score, item.token_count, vec_chunk.text_chunk
                        ));
                    }
                }
            }
        }

        if !l2_summaries.is_empty() {
            prompt.push_str("## L2 Semantic Summary Cache (Compacted Invariants)\n\n");
            for (id, summary) in &l2_summaries {
                prompt.push_str(&format!(
                    "- [{}] (Tokens: {} -> {}, Saved: {:.1}%): {}\n",
                    id,
                    summary.original_token_count,
                    summary.token_count,
                    summary.compression_ratio * 100.0,
                    summary.summary_text
                ));
            }
            prompt.push('\n');
        }

        let (l1_tokens, l2_tokens, total_tokens) = state.calculate_tokens();
        let l3_count = self.l3_store.count().unwrap_or(0);
        let downstream = state.compute_downstream_dependencies();
        let causal_protected_count = state
            .items
            .keys()
            .filter(|id| downstream.get(*id).copied().unwrap_or(0) > 0)
            .count();

        let uncompressed_total: usize = state
            .items
            .values()
            .map(|e| e.item.token_count)
            .sum();

        let overall_compression_ratio = if uncompressed_total == 0 || total_tokens >= uncompressed_total {
            0.0
        } else {
            1.0 - (total_tokens as f64 / uncompressed_total as f64)
        };

        let metrics = WorkingMemoryMetrics {
            l1_token_count: l1_tokens,
            l2_token_count: l2_tokens,
            total_active_tokens: total_tokens,
            max_token_budget: config.max_token_budget,
            l1_items_count: l1_items.len(),
            l2_items_count: l2_summaries.len(),
            l3_items_count: l3_count,
            causal_protected_items_count: causal_protected_count,
            overall_compression_ratio,
        };

        AssembledContext {
            prompt_text: prompt,
            l1_items,
            l2_summaries,
            metrics,
        }
    }

    /// Reconstitutes an item back into L1 Active Memory.
    /// Supports promoting an item from L2 or retrieving and decompressing from L3.
    pub fn reconstitute_to_l1(&self, id: &str) -> Result<bool> {
        let mut state = self.inner.write().unwrap();

        // 1. Check if item is currently in L2
        if let Some(entry) = state.items.get_mut(id) {
            if entry.tier == MemoryTier::L2SemanticSummary {
                entry.tier = MemoryTier::L1Active;
                entry.summary = None;
                return Ok(true);
            } else if entry.tier == MemoryTier::L1Active {
                return Ok(true);
            }
        }

        // 2. Check if item is in L3 cold persistent store
        if let Some(item) = self.l3_store.retrieve(id)? {
            self.l3_store.delete(id)?;
            let entry = WorkingMemoryEntry {
                item,
                tier: MemoryTier::L1Active,
                summary: None,
                last_attention: 1.0,
            };
            if !state.items.contains_key(id) {
                state.insertion_order.push(id.to_string());
            }
            state.items.insert(id.to_string(), entry);
            return Ok(true);
        }

        Ok(false)
    }

    /// Retrieves an item from any tier (L1, L2, or L3)
    pub fn get_item(&self, id: &str) -> Result<Option<(ContextItem, MemoryTier)>> {
        let state = self.inner.read().unwrap();
        if let Some(entry) = state.items.get(id) {
            return Ok(Some((entry.item.clone(), entry.tier)));
        }
        drop(state);

        if let Some(item) = self.l3_store.retrieve(id)? {
            return Ok(Some((item, MemoryTier::L3ColdPersistent)));
        }

        Ok(None)
    }

    /// Current live working memory telemetry metrics
    pub fn metrics(&self) -> WorkingMemoryMetrics {
        let state = self.inner.read().unwrap();
        let config = self.config.read().unwrap();

        let (l1_tokens, l2_tokens, total_tokens) = state.calculate_tokens();
        let l3_count = self.l3_store.count().unwrap_or(0);
        let downstream = state.compute_downstream_dependencies();
        let causal_protected_count = state
            .items
            .keys()
            .filter(|id| downstream.get(*id).copied().unwrap_or(0) > 0)
            .count();

        let mut l1_count = 0;
        let mut l2_count = 0;
        let mut uncompressed_total = 0;

        for entry in state.items.values() {
            uncompressed_total += entry.item.token_count;
            match entry.tier {
                MemoryTier::L1Active => l1_count += 1,
                MemoryTier::L2SemanticSummary => l2_count += 1,
                MemoryTier::L3ColdPersistent => {}
            }
        }

        let overall_compression_ratio = if uncompressed_total == 0 || total_tokens >= uncompressed_total {
            0.0
        } else {
            1.0 - (total_tokens as f64 / uncompressed_total as f64)
        };

        WorkingMemoryMetrics {
            l1_token_count: l1_tokens,
            l2_token_count: l2_tokens,
            total_active_tokens: total_tokens,
            max_token_budget: config.max_token_budget,
            l1_items_count: l1_count,
            l2_items_count: l2_count,
            l3_items_count: l3_count,
            causal_protected_items_count: causal_protected_count,
            overall_compression_ratio,
        }
    }

    /// Clears all working memory state across L1 and L2
    pub fn clear(&self) -> Result<()> {
        let mut state = self.inner.write().unwrap();
        state.items.clear();
        state.insertion_order.clear();
        Ok(())
    }
}

// ============================================================================
// 7. Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_item_constructors_and_token_estimation() {
        let turn = ContextItem::new_conversation(
            "conv-1",
            ConversationRole::User,
            "Developer",
            "Refactor kernel memory bus to zero-copy",
            0.9,
            vec![],
        );
        assert_eq!(turn.modality(), ModalityKind::ConversationTurn);
        assert!(turn.token_count > 0);

        let ast = ContextItem::new_ast_fragment(
            "ast-1",
            "src/bus.rs",
            "rust",
            "pub fn transfer() { println!(\"ok\"); }",
            "fn",
            true,
            0.95,
            vec!["conv-1".to_string()],
        );
        assert_eq!(ast.modality(), ModalityKind::AstSyntaxFragment);
        assert_eq!(ast.causal_parents, vec!["conv-1".to_string()]);
    }

    #[test]
    fn test_attention_decay_formula_properties() {
        let config = AttentionConfig::default();

        let base_ts = 1_000_000u64;
        let mut ast_anchor = ContextItem::new_ast_fragment(
            "anchor-1",
            "src/main.rs",
            "rust",
            "fn anchor() {}",
            "fn",
            true,
            1.0,
            vec![],
        );
        ast_anchor.timestamp_ms = base_ts;

        let mut term_log = ContextItem::new_terminal_log(
            "term-1",
            TerminalStream::Stderr,
            "cargo check",
            Some(1),
            "warning: unused variable",
            1.0,
            vec![],
        );
        term_log.timestamp_ms = base_ts;

        // Fresh items evaluated at base_ts
        let score_anchor_fresh = config.compute_attention(&ast_anchor, base_ts, 0);
        let score_term_fresh = config.compute_attention(&term_log, base_ts, 0);
        assert!(score_anchor_fresh > score_term_fresh);

        // After 100 seconds
        let eval_ts = base_ts + 100_000;
        let score_anchor_decayed = config.compute_attention(&ast_anchor, eval_ts, 0);
        let score_term_decayed = config.compute_attention(&term_log, eval_ts, 0);

        // Terminal log decays drastically faster than AST anchor
        let anchor_retention = score_anchor_decayed / score_anchor_fresh;
        let term_retention = score_term_decayed / score_term_fresh;
        assert!(
            anchor_retention > term_retention,
            "Anchor retention ({}) must be higher than terminal log retention ({})",
            anchor_retention,
            term_retention
        );

        // Downstream dependency boost
        let score_with_deps = config.compute_attention(&ast_anchor, base_ts, 4);
        assert!(score_with_deps > score_anchor_fresh);
    }

    #[test]
    fn test_l3_sqlite_roundtrip_compression() {
        let l3 = L3ColdStore::open_in_memory().unwrap();
        let item = ContextItem::new_conversation(
            "cold-1",
            ConversationRole::Assistant,
            "HagibisAgent",
            "Completed distributed memory compaction sweep across all worker nodes",
            0.8,
            vec![],
        );

        l3.persist(&item, 0.45, 1_000_000).unwrap();
        assert_eq!(l3.count().unwrap(), 1);

        let retrieved = l3.retrieve("cold-1").unwrap().expect("Item should exist");
        assert_eq!(retrieved.id, "cold-1");
        assert_eq!(retrieved.raw_content, item.raw_content);

        let deleted = l3.delete("cold-1").unwrap();
        assert!(deleted);
        assert_eq!(l3.count().unwrap(), 0);
    }
}
