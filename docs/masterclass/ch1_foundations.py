"""
ch1_foundations.py - Cover Page, Foreword, Curriculum, and Chapter 1 Foundations
"""

import os
from reportlab.lib import colors
from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, PageBreak, KeepTogether
from reportlab.platypus import Image as RLImage

from styles import (
    get_masterclass_styles, make_callout, make_code_block, make_divider,
    PRIMARY_NAVY, DARK_SLATE, CYAN_ACCENT, CYAN_DARK, CYAN_LIGHT,
    LIGHT_BORDER, BG_CARD, BG_ICE, EMERALD_GREEN, AMBER_GOLD, PURPLE_ACCENT
)

def build_cover_and_foundations():
    styles = get_masterclass_styles()
    story = []

    # =========================================================================
    # FRONT COVER PAGE
    # =========================================================================
    story.append(Spacer(1, 10))
    story.append(Paragraph("🪽 THE SOVEREIGN VIBE CODING MASTERCLASS 🪽", styles['CoverSubtitle']))
    story.append(Paragraph("HAGIBIS & HAGIBIS DAEMON", styles['CoverTitle']))
    story.append(Paragraph("<b>The Sub-Millisecond Systems Microkernel, Swarm Engine &amp; 87 Sovereign Superpowers</b>", styles['CoverSubtitle']))
    story.append(Spacer(1, 5))

    cover_img_path = "/home/dyna/TGS Projects/hagibis/docs/images/talaria_cover.jpg"
    if os.path.exists(cover_img_path):
        img = RLImage(cover_img_path, width=280, height=280)
        img.hAlign = 'CENTER'
        story.append(img)
    story.append(Spacer(1, 10))

    mascot_badge_text = (
        "<b>Official Mascot &amp; Flight Guide: Talaria — The Sovereign Winged Avatar of Swiftness</b><br/>"
        "<i>'Wear the winged golden sandals of Mercury. Eliminate binary bloat, drop IPC friction to 12 microseconds, "
        "and command the dual-brain sovereign swarm at the velocity of thought.'</i>"
    )
    story.append(make_callout("COGNITIVE COMPANION BRIEFING", mascot_badge_text, kind="wisdom", width=532))
    story.append(Spacer(1, 10))

    spec_data = [
        [
            Paragraph("<b>Architecture:</b> Systems-Grade Rust Microkernel", styles['TableCell']),
            Paragraph("<b>Binary Footprint:</b> hgb (939 KB), hgbd (677 KB)", styles['TableCell']),
            Paragraph("<b>IPC Latency:</b> 12 µs Unix Domain Sockets", styles['TableCell']),
        ],
        [
            Paragraph("<b>Daemon RSS:</b> 8.4 MB (Sub-10MB Resident)", styles['TableCell']),
            Paragraph("<b>Dual-Brain:</b> Offline Ollama + Gemini Cloud", styles['TableCell']),
            Paragraph("<b>Checkpoints:</b> &lt;10µs CoW WAL Time Machine", styles['TableCell']),
        ],
        [
            Paragraph("<b>Sovereign Superpowers:</b> 87 Fully Verified", styles['TableCell']),
            Paragraph("<b>Real-World Scenarios:</b> 500 Concrete Playbooks", styles['TableCell']),
            Paragraph("<b>Edition:</b> Sovereign Edition 1.0 (2026)", styles['TableCell']),
        ]
    ]
    spec_table = Table(spec_data, colWidths=[177, 177, 178])
    spec_table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, -1), BG_CARD),
        ('BOX', (0, 0), (-1, -1), 0.75, LIGHT_BORDER),
        ('INNERGRID', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('TOPPADDING', (0, 0), (-1, -1), 4),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 4),
        ('LEFTPADDING', (0, 0), (-1, -1), 6),
        ('RIGHTPADDING', (0, 0), (-1, -1), 6),
    ]))
    story.append(spec_table)

    story.append(Spacer(1, 10))
    story.append(Paragraph("<font color='#64748b'><b>Authored by the Lead Technical Writer &amp; Vibe Systems Architect</b> • Published for Sovereign Vibe Developers Worldwide</font>", styles['ScenarioMeta']))
    
    story.append(PageBreak())

    # =========================================================================
    # EXECUTIVE CURRICULUM OVERVIEW & TABLE OF CONTENTS
    # =========================================================================
    story.append(Paragraph("Executive Curriculum Overview", styles['ChapterHeading']))
    story.append(Paragraph(
        "This masterclass is the definitive systems and pedagogical reference manual for <b>Hagibis (hgb)</b> "
        "and the <b>Hagibis Resident Daemon (hgbd)</b>. It guides developers through the architecture, interactive tooling, "
        "all 87 sovereign capabilities, five real-world tutorials, and exactly 500 production scenarios.",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    curriculum_data = [
        [
            Paragraph("<b>Module</b>", styles['TableHeader']),
            Paragraph("<b>Title &amp; Focus Area</b>", styles['TableHeader']),
            Paragraph("<b>Key Deliverables &amp; Core Competencies</b>", styles['TableHeader'])
        ],
        [
            Paragraph("<b>Foreword</b>", styles['TableCellBold']),
            Paragraph("The Sub-Millisecond Manifesto by Talaria", styles['TableCell']),
            Paragraph("Philosophical transformation, flow state psychology, and stripping binary bloat.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 1</b>", styles['TableCellBold']),
            Paragraph("The Vibe Coding Paradigm &amp; Foundations", styles['TableCell']),
            Paragraph("Context elimination, hallucination extermination, speculative dual-draft racing.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 2</b>", styles['TableCellBold']),
            Paragraph("Dual-Engine Systems Architecture (hgb &amp; hgbd)", styles['TableCell']),
            Paragraph("Tokio UDS microkernel, 12µs Bincode IPC, SIMD vector memory, SQLite CoW sandboxes.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 3</b>", styles['TableCellBold']),
            Paragraph("Conversational Cockpit &amp; Slash Commands", styles['TableCell']),
            Paragraph("Ratatui TUI canvas, differential cards, full reference of 70+ interactive slash commands.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 4</b>", styles['TableCellBold']),
            Paragraph("The 87 Sovereign Superpowers Technical Reference", styles['TableCell']),
            Paragraph("Comprehensive signatures, flags, inputs/outputs, and mechanics across 7 sovereign tiers.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 5</b>", styles['TableCellBold']),
            Paragraph("Five End-to-End Production Tutorials", styles['TableCell']),
            Paragraph("Fullstack Next.js+Axum, Sentry triage hotfixes, offline dual-drafting, multi-repo, SaaS.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 6</b>", styles['TableCellBold']),
            Paragraph("The 500 Real-World Scenarios Masterclass", styles['TableCell']),
            Paragraph("500 distinct, concrete, categorized playbooks across 10 mission-critical domains.", styles['TableCell'])
        ],
        [
            Paragraph("<b>Chapter 7</b>", styles['TableCellBold']),
            Paragraph("Appendix, CLI Manual &amp; The Vibe Coder's Oath", styles['TableCell']),
            Paragraph("Environment variables, daemon lifecycle, troubleshooting, and sovereign code of ethics.", styles['TableCell'])
        ]
    ]

    curr_table = Table(curriculum_data, colWidths=[70, 200, 262])
    curr_table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), PRIMARY_NAVY),
        ('BOX', (0, 0), (-1, -1), 0.75, DARK_SLATE),
        ('INNERGRID', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, BG_ICE]),
        ('TOPPADDING', (0, 0), (-1, -1), 4),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 4),
        ('LEFTPADDING', (0, 0), (-1, -1), 6),
        ('RIGHTPADDING', (0, 0), (-1, -1), 6),
    ]))
    story.append(curr_table)
    story.append(Spacer(1, 8))

    # =========================================================================
    # FOREWORD BY TALARIA
    # =========================================================================
    story.append(Paragraph("Foreword by Talaria: The Sub-Millisecond Manifesto", styles['SectionHeading']))
    story.append(Paragraph(
        "<i>'Listen closely, fellow builder. The greatest impediment to software engineering has never been syntax, "
        "type systems, or algorithm complexity. The true killer of great software has always been friction.'</i>",
        styles['BodyBold']
    ))
    story.append(Paragraph(
        "For decades, software development has been bogged down by friction: waiting 45 seconds for a webpack build, "
        "wrestling with 100 megabyte Node/Python runtimes, copy-pasting cryptic stack traces from terminal tabs into chat windows, "
        "and praying that an autonomous agent wouldn't hallucinate non-existent NPM libraries or trash a working git workspace. "
        "Every time you leave your editor to paste an error into a browser LLM, your working memory resets. Your flow state evaporates.",
        styles['Body']
    ))
    story.append(Paragraph(
        "In Roman mythology, Hermes (Mercury) traversed the cosmos not by walking or straining, but by donning <b>Talaria</b>—the "
        "winged golden sandals crafted by Hephaestus. With them, distance vanished, gravity lost its hold, and the messenger arrived "
        "before mortals took their first stride. In the Philippines, <b>Hagibis</b> signifies supreme velocity paired with unstoppable force—the "
        "sudden rush of wind and thunder.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Hagibis is the Winged Sandal of the modern Vibe Coder.</b> By building a resident systems-grade microkernel in pure Rust, "
        "operating with 12-microsecond Unix Domain Socket IPC, and providing 87 sovereign developer superpowers, Hagibis moves "
        "faster than your doubts. When you code with Hagibis, you don't wait for your tools—your tools run ahead of your imagination.",
        styles['Body']
    ))
    
    story.append(make_callout(
        "THE FIRST LAW OF VIBE CODING",
        "If a developer tool takes more than 100 milliseconds to respond, it is not a partner in creative thought—it is a bureaucratic roadblock. "
        "Hagibis enforces sub-millisecond local latency so your cognitive momentum is never interrupted.",
        kind="wisdom",
        width=532
    ))

    story.append(PageBreak())

    # =========================================================================
    # CHAPTER 1: THE VIBE CODING PARADIGM & PHILOSOPHICAL FOUNDATIONS
    # =========================================================================
    story.append(Paragraph("Chapter 1: The Vibe Coding Paradigm &amp; Philosophical Foundations", styles['ChapterHeading']))
    story.append(make_divider(color=CYAN_ACCENT, thickness=1.5, space_before=2, space_after=8))

    story.append(Paragraph("1.1 What is Vibe Coding?", styles['SectionHeading']))
    story.append(Paragraph(
        "<b>Vibe Coding</b> represents the paradigm shift from mechanical syntax transcription to high-level architectural orchestration. "
        "In traditional programming, 80% of a developer's time is spent wrestling with boilerplate, reading API documentation, "
        "debugging missing imports, configuring build tools, and context-switching between IDE, browser DevTools, and terminal tabs. "
        "Only 20% is spent on creative intent and structural design.",
        styles['Body']
    ))
    story.append(Paragraph(
        "Vibe Coding inverts this ratio completely. The developer operates in an uninterrupted <b>Flow State</b>, expressing intent "
        "through natural language, visual gestures, voice commands, and interactive canvas manipulation. The underlying engine "
        "autonomously handles AST parsing, type checking, test synthesis, dependency resolution, and runtime verification.",
        styles['Body']
    ))

    story.append(Paragraph("1.2 The Anatomy of Friction: Why Traditional AI Tooling Fails", styles['SectionHeading']))
    story.append(Paragraph(
        "Most first-generation 'AI coding assistants' were built as wrappers around HTTP chat endpoints. They suffer from four catastrophic flaws:",
        styles['Body']
    ))
    story.append(Paragraph("• <b>Bloated Runtimes &amp; High RSS:</b> Electron and Python-based tools frequently consume 1.5 GB to 4 GB of RAM, causing thermal throttling, fan noise, and battery drain.", styles['BulletText']))
    story.append(Paragraph("• <b>Glacial Local Latency:</b> Round-trip HTTP serialization, unoptimized JSON framing, and remote network hops inject 1 to 3 seconds of lag before the first token streams.", styles['BulletText']))
    story.append(Paragraph("• <b>Hallucinated Packages &amp; Slopsquatting:</b> Blind autocomplete engines invent non-existent package names, introducing severe supply-chain vulnerabilities.", styles['BulletText']))
    story.append(Paragraph("• <b>Destructive Edits &amp; State Anxiety:</b> Agents mutate source code without atomic rollbacks, leaving developers terrified that a bad generation will corrupt their git history.", styles['BulletText']))

    story.append(Spacer(1, 4))
    story.append(Paragraph("<b>Table 1.1: Systems Comparison — Traditional AI Tooling vs. Hagibis Vibe Engine</b>", styles['BodyBold']))
    
    comp_data = [
        [
            Paragraph("<b>Evaluation Dimension</b>", styles['TableHeader']),
            Paragraph("<b>Traditional AI Coding Tools</b>", styles['TableHeader']),
            Paragraph("<b>🪽 Hagibis (hgb / hgbd) Vibe Engine</b>", styles['TableHeader'])
        ],
        [
            Paragraph("<b>Binary Footprint</b>", styles['TableCellBold']),
            Paragraph("40 MB – 150 MB (Node/Python/Electron)", styles['TableCell']),
            Paragraph("Sub-1MB Rust binaries (hgb: 939 KB, hgbd: 677 KB)", styles['TableCellCode'])
        ],
        [
            Paragraph("<b>Resident Memory (RSS)</b>", styles['TableCellBold']),
            Paragraph("600 MB – 2,400 MB RAM", styles['TableCell']),
            Paragraph("8.4 MB Resident Microkernel (Tokio epoll)", styles['TableCellCode'])
        ],
        [
            Paragraph("<b>IPC Communication</b>", styles['TableCellBold']),
            Paragraph("HTTP/REST over TCP loopback (15–40 ms)", styles['TableCell']),
            Paragraph("12 µs Unix Domain Sockets + Bincode Zero-Copy", styles['TableCellCode'])
        ],
        [
            Paragraph("<b>Model Sovereignty</b>", styles['TableCellBold']),
            Paragraph("Locked into proprietary cloud subscription", styles['TableCell']),
            Paragraph("Intelligent Dual-Brain: Offline Ollama &amp; Gemini Cloud", styles['TableCellCode'])
        ],
        [
            Paragraph("<b>Hallucination Prevention</b>", styles['TableCellBold']),
            Paragraph("None; relies on manual user inspection", styles['TableCell']),
            Paragraph("Speculative First-Green Racing + Blake3 Merkle Gates", styles['TableCellCode'])
        ],
        [
            Paragraph("<b>State Recovery</b>", styles['TableCellBold']),
            Paragraph("Manual git stash / git reset disaster recovery", styles['TableCell']),
            Paragraph("&lt;10µs Atomic CoW DB &amp; ChronoWarp 4D Rollback", styles['TableCellCode'])
        ],
        [
            Paragraph("<b>Error Telemetry</b>", styles['TableCellBold']),
            Paragraph("Copy-pasting stack traces into chat prompt", styles['TableCell']),
            Paragraph("Ambient Browser Snoop, CDP Wiretap &amp; Shell Interceptor", styles['TableCellCode'])
        ]
    ]

    comp_table = Table(comp_data, colWidths=[120, 206, 206])
    comp_table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), PRIMARY_NAVY),
        ('BOX', (0, 0), (-1, -1), 0.75, DARK_SLATE),
        ('INNERGRID', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, BG_ICE]),
        ('TOPPADDING', (0, 0), (-1, -1), 3.5),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 3.5),
        ('LEFTPADDING', (0, 0), (-1, -1), 6),
        ('RIGHTPADDING', (0, 0), (-1, -1), 6),
    ]))
    story.append(comp_table)
    story.append(Spacer(1, 6))

    story.append(Paragraph("1.3 Eliminating Context Switching and Hallucinations", styles['SectionHeading']))
    story.append(Paragraph(
        "Hagibis eradicates hallucinations through a multi-layered defense fabric built into the daemon kernel:",
        styles['Body']
    ))
    story.append(Paragraph(
        "1. <b>Speculative Dual-Draft Racing ('First Green Wins'):</b> When a prompt is submitted, Hagibis dispatches it concurrently to a high-speed local offline model (e.g. Qwen 2.5 Coder 7B) and a frontier cloud model (Gemini 2.5 Pro). The daemon executes the compiler and test harness against both candidate drafts in ephemeral memory-backed sandboxes. The first draft that passes type checks and test invariants lands immediately. If the local model succeeds in 180 ms, you never wait for cloud latency.",
        styles['Body']
    ))
    story.append(Paragraph(
        "2. <b>AST Slicing &amp; Symbol Pinpointing:</b> Instead of feeding entire 5,000-line source files into the prompt context, the Hagibis AST slicer extracts only the targeted function, its immediate caller graph, and relevant struct definitions. This prevents context saturation, cuts token expenditure by 85%, and eliminates off-target modifications.",
        styles['Body']
    ))
    story.append(Paragraph(
        "3. <b>Slopsquatting &amp; Supply-Chain Firewall:</b> When an agent proposes adding a new dependency (e.g., `npm i fancy-jwt-validator` or `cargo add super-auth`), the daemon intercepts the request before touching disk. It validates package creation dates, download metrics, and registry ownership against official crates.io and npm registries, rejecting hallucinated hallucination vectors automatically.",
        styles['Body']
    ))

    story.append(make_callout(
        "THE HALLUCINATION FIREWALL IN ACTION",
        "Never accept code generated in a vacuum. Hagibis verifies every single AST mutation against concrete golden invariants "
        "before it touches your working tree. If code cannot compile, it never enters your repository.",
        kind="tip",
        width=532
    ))

    return story
