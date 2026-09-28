"""
ch2_architecture.py - Chapter 2: The Dual-Engine Systems Architecture (hgb & hgbd)
"""

import os
from reportlab.lib import colors
from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, PageBreak, KeepTogether
from reportlab.platypus import Image as RLImage

from styles import (
    get_masterclass_styles, make_callout, make_code_block, make_divider,
    PRIMARY_NAVY, DARK_SLATE, CYAN_ACCENT, CYAN_DARK, CYAN_LIGHT,
    LIGHT_BORDER, BG_CARD, EMERALD_GREEN, AMBER_GOLD, PURPLE_ACCENT
)

def build_architecture_chapter():
    styles = get_masterclass_styles()
    story = []

    story.append(Paragraph("Chapter 2: Dual-Engine Systems Architecture (hgb &amp; hgbd)", styles['ChapterHeading']))
    story.append(make_divider(color=CYAN_ACCENT, thickness=1.5, space_before=2, space_after=8))

    story.append(Paragraph(
        "At the architectural core of Hagibis lies a clean separation of concerns: a lightning-fast client interface "
        "(<code>hgb</code>) paired with a persistent, resident systems microkernel daemon (<code>hgbd</code>). "
        "Unlike monolithic scripting tools that pay a 2-second interpreter startup tax on every keystroke, Hagibis leverages "
        "sub-millisecond Unix Domain Sockets and zero-copy binary serialization.",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    arch_img_path = "/home/dyna/TGS Projects/hagibis/docs/images/talaria_architecture.jpg"
    if os.path.exists(arch_img_path):
        img = RLImage(arch_img_path, width=490, height=273)
        img.hAlign = 'CENTER'
        story.append(img)
        story.append(Spacer(1, 4))
        story.append(Paragraph(
            "<font color='#64748b'><b>Figure 2.1:</b> Dual-Engine Microkernel Architecture — Unix Domain Socket IPC, "
            "Tokio Actor Pipeline, SIMD Vector Memory, and Copy-on-Write SQLite Sandboxes.</font>",
            styles['ScenarioMeta']
        ))
    story.append(Spacer(1, 6))

    story.append(Paragraph("2.1 The Two Binaries: hgb vs. hgbd", styles['SectionHeading']))
    story.append(Paragraph(
        "Hagibis compiles into two compact, statically linked Rust binaries with zero external runtime dependencies:",
        styles['Body']
    ))
    story.append(Paragraph(
        "• <b><code>hgb</code> (The Thin Client &amp; Cockpit Canvas):</b> Sized at just <b>939 KB</b>, <code>hgb</code> is responsible "
        "for CLI argument parsing, interactive Ratatui terminal UI rendering, terminal raw mode handling, mouse interaction, "
        "and client-side stream visualization. It starts in under 2 milliseconds and transmits requests to the daemon via IPC.",
        styles['BulletText']
    ))
    story.append(Paragraph(
        "• <b><code>hgbd</code> (The Resident Microkernel Daemon):</b> Sized at <b>677 KB</b> with a steady-state memory RSS of "
        "<b>8.4 MB</b>, <code>hgbd</code> runs continuously in the background. It manages persistent model connections, "
        "file system watchers, AST index graphs, background speculative race workers, and SQLite state ledgers.",
        styles['BulletText']
    ))

    story.append(Spacer(1, 4))
    story.append(Paragraph("2.2 High-Speed IPC: Unix Domain Sockets &amp; Bincode Serialization", styles['SectionHeading']))
    story.append(Paragraph(
        "Traditional agentic developer tools rely on HTTP/REST or JSON-RPC over loopback TCP (<code>http://127.0.0.1:port</code>). "
        "This introduces TCP three-way handshake overhead, loopback socket buffer copies, and heavy JSON text serialization latency (often 10–35 ms).",
        styles['Body']
    ))
    story.append(Paragraph(
        "Hagibis eliminates this bottleneck entirely. All communication between <code>hgb</code> and <code>hgbd</code> flows through "
        "a native <b>Unix Domain Socket (UDS)</b> located at <code>/tmp/hgbd.sock</code> (or <code>$XDG_RUNTIME_DIR/hgb/hgbd.sock</code>). "
        "Payloads are serialized using <b>Bincode</b>—a compact, zero-overhead binary encoder that maps directly into Rust memory structures. "
        "The measured round-trip IPC latency across the socket is a staggering <b>12 microseconds (0.012 milliseconds)</b>.",
        styles['Body']
    ))

    story.append(make_code_block("""// Crates/hgb-core/src/protocol.rs: Zero-Copy Binary Framing
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HgbRequest {
    Ping,
    Status,
    Prompt { prompt: String, model: Option<String>, provider: Option<String>, stream: bool },
    VibeRace { prompt: String, target_dir: Option<String> },
    CdpTeleportResolve { selector: String },
    DbCowSnapshotCreate { db_path: String, description: String },
    // ... 87 Sovereign Superpowers Enum Variants
}

// Client UDS Dispatch (crates/hgb-cli/src/client.rs)
pub async fn send_request(sock: &Path, req: &HgbRequest) -> Result<HgbResponse, HgbError> {
    let mut stream = UnixStream::connect(sock).await?;
    let payload = bincode::serialize(req)?;
    stream.write_u32(payload.len() as u32).await?;
    stream.write_all(&payload).await?;
    // Response deserialization in sub-15 microseconds
    let len = stream.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(bincode::deserialize(&buf)?)
}""", caption="Bincode Binary Serialization over Unix Domain Sockets"))

    story.append(Spacer(1, 6))
    story.append(Paragraph("2.3 Tokio Actor Loop &amp; Memory Architecture", styles['SectionHeading']))
    story.append(Paragraph(
        "Inside <code>hgbd</code>, a high-throughput Tokio event loop schedules asynchronous tasks using native epoll (Linux) "
        "or kqueue (macOS/BSD). The daemon maintains four core in-memory subsystems:",
        styles['Body']
    ))
    story.append(Paragraph("1. <b>SIMD-Accelerated Vector Memory:</b> Workspace symbol embeddings are stored in memory-aligned AVX-512 / ARM Neon buffers for sub-millisecond semantic search without external vector databases.", styles['BulletText']))
    story.append(Paragraph("2. <b>Blake3 Merkle Provenance Ledger:</b> Every AST transformation and patch is cryptographically hashed with Blake3, generating an immutable, tamper-evident audit trail of all AI interventions.", styles['BulletText']))
    story.append(Paragraph("3. <b>Ephemeral CoW SQLite Sandboxes:</b> When executing speculative code or running database migrations, the daemon clones state using OS-level Copy-on-Write pages. Checkpoint rollbacks execute in under 10 microseconds.", styles['BulletText']))
    story.append(Paragraph("4. <b>Style Memory &amp; Reject-Learner Vault:</b> When you reject or modify an agent suggestion, the daemon extracts the negative pattern and updates the style guidance matrix instantly.", styles['BulletText']))

    story.append(Spacer(1, 4))
    story.append(Paragraph("2.4 Auto-Spawning Lifecycle &amp; Standalone Fallback", styles['SectionHeading']))
    story.append(Paragraph(
        "Hagibis requires zero manual daemon administration. When a developer executes any <code>hgb</code> command:",
        styles['Body']
    ))
    story.append(Paragraph("1. <code>hgb</code> probes the UDS socket with a 500µs handshake ping.", styles['BulletText']))
    story.append(Paragraph("2. If the daemon is not running, <code>hgb</code> seamlessly forks and detaches <code>hgbd</code> as a background resident service.", styles['BulletText']))
    story.append(Paragraph("3. If daemon spawning is restricted by container policies (e.g. locked Docker sandboxes), <code>hgb</code> transparently activates <b>Standalone Fallback Mode</b>, executing all microkernel logic in-process without failing.", styles['BulletText']))

    story.append(make_callout(
        "DAEMON RESILIENCE GUARANTEE",
        "The daemon is completely stateless with respect to your working code—all persistent memory is committed to WAL-backed SQLite "
        "and append-only git logs. If the daemon process is forcefully killed (`kill -9 $(pgrep hgbd)`), the next `hgb` invocation "
        "reconnects and recovers state in under 8 milliseconds.",
        kind="note",
        width=532
    ))

    return story
