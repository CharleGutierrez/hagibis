"""
ch3_cockpit.py - Chapter 3: Conversational Cockpit & Interactive REPL Canvas Reference
"""

import os
from reportlab.lib import colors
from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, PageBreak, KeepTogether

from styles import (
    get_masterclass_styles, make_callout, make_code_block, make_divider,
    PRIMARY_NAVY, DARK_SLATE, CYAN_ACCENT, CYAN_DARK, CYAN_LIGHT,
    LIGHT_BORDER, BG_CARD, BG_ICE, EMERALD_GREEN, AMBER_GOLD, PURPLE_ACCENT
)

def build_cockpit_chapter():
    styles = get_masterclass_styles()
    story = []

    story.append(Paragraph("Chapter 3: Interactive REPL Canvas &amp; Slash Commands Reference", styles['ChapterHeading']))
    story.append(make_divider(color=CYAN_ACCENT, thickness=1.5, space_before=2, space_after=8))

    story.append(Paragraph(
        "Hagibis introduces the <b>Conversational Cockpit</b>: a high-density, terminal-native workstation powered by "
        "Ratatui and Crossterm. Rather than forcing developers to switch between chat windows, code editors, and command prompts, "
        "the Cockpit merges them into a unified, reactive canvas.",
        styles['Body']
    ))

    story.append(Paragraph("3.1 The Three Cockpit Operating Modes", styles['SectionHeading']))
    story.append(Paragraph(
        "Depending on developer workflow preferences and environment constraints, Hagibis operates in three distinct modes:",
        styles['Body']
    ))
    story.append(Paragraph(
        "1. <b>Full-Screen Cockpit TUI (<code>hgb chat</code> or default <code>hgb</code>):</b> "
        "A full-screen Ratatui terminal application featuring split-pane diff cards, active AST breadcrumbs, real-time "
        "token generation speedometers (e.g. <code>42.8 tok/s</code>), syntax-highlighted code windows, and full mouse scroll/click support.",
        styles['BulletText']
    ))
    story.append(Paragraph(
        "2. <b>Classic Line-by-Line REPL (<code>hgb classic</code> or <code>hgb repl</code>):</b> "
        "A lightweight, Unix-standard scrolling terminal REPL with animated ANSI progress waves, inline Markdown parsing, "
        "command history navigation, and tab-completion. Ideal for SSH sessions or minimalist setups.",
        styles['BulletText']
    ))
    story.append(Paragraph(
        "3. <b>Headless Automation Mode (<code>hgb [options] &lt;subcommand&gt;</code>):</b> "
        "Direct CLI invocation for scripts, CI/CD runners, and git pre-commit hooks, producing machine-readable JSON or streaming raw text.",
        styles['BulletText']
    ))

    story.append(Spacer(1, 4))
    story.append(Paragraph("3.2 Anatomy of the Cockpit Interface", styles['SectionHeading']))
    story.append(Paragraph(
        "The full Cockpit canvas is divided into four synchronized quadrants:",
        styles['Body']
    ))
    story.append(Paragraph("• <b>Telemetry Header:</b> Displays daemon uptime, memory RSS, active AI model badge, and round-trip UDS latency.", styles['BulletText']))
    story.append(Paragraph("• <b>Timeline &amp; Conversation Stream:</b> Shows the interactive dialog between developer and agents, with tool execution cards.", styles['BulletText']))
    story.append(Paragraph("• <b>Differential Inspector:</b> Visualizes code mutations side-by-side with color-coded additions (green) and deletions (red).", styles['BulletText']))
    story.append(Paragraph("• <b>Multi-Command Input Bar:</b> Supports natural language intents, file references (<code>@src/main.rs</code>), and instant slash commands.", styles['BulletText']))

    story.append(Spacer(1, 6))
    story.append(Paragraph("3.3 Comprehensive Slash Command Technical Reference", styles['SectionHeading']))
    story.append(Paragraph(
        "Slash commands provide direct, zero-token access to the Hagibis microkernel and its sovereign superpowers. "
        "The following table provides the exhaustive technical reference of all slash commands supported in the Cockpit:",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    slash_commands_data = [
        [
            Paragraph("<b>Slash Command</b>", styles['TableHeader']),
            Paragraph("<b>Syntax &amp; Parameters</b>", styles['TableHeader']),
            Paragraph("<b>Description &amp; Operational Outcome</b>", styles['TableHeader'])
        ],
        # Core Flow
        [
            Paragraph("<code>/model</code>", styles['TableCellCode']),
            Paragraph("<code>/model [name | auto]</code>", styles['TableCell']),
            Paragraph("Dynamically switch active AI model between local Ollama and Gemini Cloud without session restart.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/vibe</code>", styles['TableCellCode']),
            Paragraph("<code>/vibe &lt;prompt&gt;</code>", styles['TableCell']),
            Paragraph("Execute speculative dual-draft race (First Green Wins) across local and cloud models simultaneously.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/forge</code>", styles['TableCellCode']),
            Paragraph("<code>/forge &lt;stack&gt; &lt;name&gt;</code>", styles['TableCell']),
            Paragraph("Scaffold production fullstack applications in under 2 seconds (axum, ratatui, vite, next).", styles['TableCell'])
        ],
        [
            Paragraph("<code>/blueprint</code>", styles['TableCellCode']),
            Paragraph("<code>/blueprint [--format svg|dag]</code>", styles['TableCell']),
            Paragraph("Render living ASCII/Mermaid architectural dependency graph and component blast radius.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/redteam</code>", styles['TableCellCode']),
            Paragraph("<code>/redteam [path]</code>", styles['TableCell']),
            Paragraph("Deep architectural security audit for auth bypasses, O(N²) loops, and leaked secrets.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/rewind</code>", styles['TableCellCode']),
            Paragraph("<code>/rewind &lt;symbol_name&gt;</code>", styles['TableCell']),
            Paragraph("Surgically roll back a single function, struct, or component to its previous green state.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/mock</code>", styles['TableCellCode']),
            Paragraph("<code>/mock [resource] [--port N]</code>", styles['TableCell']),
            Paragraph("Spawn instant in-memory CRUD REST mock server with relational relational seed data under 15ms.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/ship</code>", styles['TableCellCode']),
            Paragraph("<code>/ship [--dry-run]</code>", styles['TableCell']),
            Paragraph("Generate atomic conventional commits, check secret leaks, and synthesize executive PR_STORY.md.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/checkpoint</code>", styles['TableCellCode']),
            Paragraph("<code>/checkpoint [label]</code>", styles['TableCell']),
            Paragraph("Commit working directory into append-only WAL checkpoint for sub-10µs rollback.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/undo</code>", styles['TableCellCode']),
            Paragraph("<code>/undo [checkpoint_id]</code>", styles['TableCell']),
            Paragraph("Instant sub-10µs atomic rollback to the previous checkpoint without leaving git residue.", styles['TableCell'])
        ],
        # Swarm & Orchestration
        [
            Paragraph("<code>/swarm</code>", styles['TableCellCode']),
            Paragraph("<code>/swarm &lt;task_description&gt;</code>", styles['TableCell']),
            Paragraph("Dispatch 4-role concurrent swarm pod (Architect, Coder, Reviewer, Tester) in parallel.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/nightshift</code>", styles['TableCellCode']),
            Paragraph("<code>/nightshift &lt;goal&gt;</code>", styles['TableCell']),
            Paragraph("Launch autonomous background worktree pipeline executing multi-step feature branches while you sleep.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/federate</code>", styles['TableCellCode']),
            Paragraph("<code>/federate &lt;goal&gt;</code>", styles['TableCell']),
            Paragraph("Orchestrate synchronized cross-repository feature branches and contract verification across monorepos.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/chaos</code>", styles['TableCellCode']),
            Paragraph("<code>/chaos &lt;target_component&gt;</code>", styles['TableCell']),
            Paragraph("Inject in-process chaos monkey experiments, jitter, and idempotency fuzzing into microservices.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/mirage</code>", styles['TableCellCode']),
            Paragraph("<code>/mirage &lt;endpoint&gt; [GET|POST]</code>", styles['TableCell']),
            Paragraph("Simulate external 3rd-party APIs (Stripe, GitHub, Twilio) offline with zero credentials.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/timewarp</code>", styles['TableCellCode']),
            Paragraph("<code>/timewarp [months_count]</code>", styles['TableCell']),
            Paragraph("Synthesize multi-month relational time-series seed data with realistic clock skews and churn.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/guardrails</code>", styles['TableCellCode']),
            Paragraph("<code>/guardrails [path]</code>", styles['TableCell']),
            Paragraph("Enforce structural architectural invariants, layer boundaries, and anti-spaghetti isolation.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/triage</code>", styles['TableCellCode']),
            Paragraph("<code>/triage &lt;raw_trace&gt;</code>", styles['TableCell']),
            Paragraph("Parse panic/Sentry crash traces, locate faulty line, synthesize regression test, and apply hotfix.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/deflake</code>", styles['TableCellCode']),
            Paragraph("<code>/deflake &lt;test_name&gt;</code>", styles['TableCell']),
            Paragraph("Stress-fuzz flaky tests 50x under simulated CPU jitter to expose and exterminate race conditions.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/anchor</code>", styles['TableCellCode']),
            Paragraph("<code>/anchor</code>", styles['TableCell']),
            Paragraph("Synthesize photographic 200-token prompt anchor capturing all active architectural decisions.", styles['TableCell'])
        ],
        # Visual & DevTools
        [
            Paragraph("<code>/teleport</code>", styles['TableCellCode']),
            Paragraph("<code>/teleport &lt;dom_selector&gt;</code>", styles['TableCell']),
            Paragraph("Click visual DOM element in browser and immediately teleport cursor to exact JSX/HTML source line.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/canvas</code>", styles['TableCellCode']),
            Paragraph("<code>/canvas &lt;old_class&gt; &lt;new_class&gt;</code>", styles['TableCell']),
            Paragraph("Two-way visual canvas: mutate Tailwind/CSS classes directly on disk without LLM token roundtrips.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/tweak</code>", styles['TableCellCode']),
            Paragraph("<code>/tweak &lt;selector&gt; &lt;prop&gt; &lt;val&gt;</code>", styles['TableCell']),
            Paragraph("Synchronize live browser DevTools visual tweaks back into repository source code bidirectionally.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/dom</code>", styles['TableCellCode']),
            Paragraph("<code>/dom [template_file]</code>", styles['TableCell']),
            Paragraph("Inspect DOM hierarchy, click coordinates, and element bounding boxes for component mapping.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/vision</code>", styles['TableCellCode']),
            Paragraph("<code>/vision &lt;prompt&gt; [image_path]</code>", styles['TableCell']),
            Paragraph("Multimodal screenshot ingestion: convert UI mockups and design screenshots into production code.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/annotate</code>", styles['TableCellCode']),
            Paragraph("<code>/annotate &lt;svg_or_box_spec&gt;</code>", styles['TableCell']),
            Paragraph("Parse visual bounding box annotations and synthesize target components directly from clipboard.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/hud</code>", styles['TableCellCode']),
            Paragraph("<code>/hud [--port N]</code>", styles['TableCell']),
            Paragraph("Launch embedded Webview HUD sidecar canvas for real-time visual inspection alongside terminal.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/preview</code>", styles['TableCellCode']),
            Paragraph("<code>/preview [--port N]</code>", styles['TableCell']),
            Paragraph("Start embedded visual live-preview sidecar proxying the local frontend devserver.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/share</code>", styles['TableCellCode']),
            Paragraph("<code>/share [port] [--slug name]</code>", styles['TableCell']),
            Paragraph("Create instant encrypted public tunnel to share your local dev server with teammates or mobile devices.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/tape</code>", styles['TableCellCode']),
            Paragraph("<code>/tape [url] &lt;scenario_name&gt;</code>", styles['TableCell']),
            Paragraph("Record headless browser screenplay and synthesize an animated PR verification tape for PR review.", styles['TableCell'])
        ],
        # Code Intelligence & Quality
        [
            Paragraph("<code>/lsp</code>", styles['TableCellCode']),
            Paragraph("<code>/lsp &lt;prefix_code&gt;</code>", styles['TableCell']),
            Paragraph("Query Universal LSP Ghost Daemon for sub-20ms inline code completions and type diagnostics.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/compact</code>", styles['TableCellCode']),
            Paragraph("<code>/compact [max_tokens]</code>", styles['TableCell']),
            Paragraph("Perform semantic rolling compaction on active session turns, preserving architectural anchors.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/commit</code>", styles['TableCellCode']),
            Paragraph("<code>/commit &lt;intent&gt;</code>", styles['TableCell']),
            Paragraph("Generate atomic conventional micro-commit directly from staged diffs with verified checks.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/recipe</code>", styles['TableCellCode']),
            Paragraph("<code>/recipe [list | run &lt;name&gt;]</code>", styles['TableCell']),
            Paragraph("Execute declarative vibe recipes and automated multi-step runbooks for repetitive engineering tasks.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/contract</code>", styles['TableCellCode']),
            Paragraph("<code>/contract &lt;symbol_name&gt;</code>", styles['TableCell']),
            Paragraph("Synthesize behavioral contract matrix (valid, invalid, boundary cases) before generating code.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/graph</code>", styles['TableCellCode']),
            Paragraph("<code>/graph &lt;goal&gt;</code>", styles['TableCell']),
            Paragraph("Query real-time live flight-graph visualizer showing agent execution subtasks and progress.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/shadow</code>", styles['TableCellCode']),
            Paragraph("<code>/shadow &lt;file_path&gt;</code>", styles['TableCell']),
            Paragraph("Execute silent speculative repair in a pre-flight shadow sandbox before altering repository disk.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/squeeze</code>", styles['TableCellCode']),
            Paragraph("<code>/squeeze [terminal_output]</code>", styles['TableCell']),
            Paragraph("Compress 20,000 lines of noisy compiler spew into a high-signal 5-line semantic error digest.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/mutation</code>", styles['TableCellCode']),
            Paragraph("<code>/mutation [file_name]</code>", styles['TableCell']),
            Paragraph("Perform anti-placebo mutation testing to ensure unit tests fail when code invariants are broken.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/mcp-hub</code>", styles['TableCellCode']),
            Paragraph("<code>/mcp-hub [list | call &lt;tool&gt;]</code>", styles['TableCell']),
            Paragraph("Discover, manage, and orchestrate external MCP tools across namespaced MCP server daemons.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/live-graph</code>", styles['TableCellCode']),
            Paragraph("<code>/live-graph [sync]</code>", styles['TableCell']),
            Paragraph("Perform real-time incremental AST graph indexing across all workspace symbols and dependencies.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/panic-fix</code>", styles['TableCellCode']),
            Paragraph("<code>/panic-fix &lt;failed_command&gt;</code>", styles['TableCell']),
            Paragraph("Intercept terminal shell panics, diagnose compiler errors, and apply 1-key automated fixes.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/plan-spec</code>", styles['TableCellCode']),
            Paragraph("<code>/plan-spec &lt;intent&gt;</code>", styles['TableCell']),
            Paragraph("Decompose natural language feature requests into Spec -> Plan -> Diff execution milestones.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/at-expand</code>", styles['TableCellCode']),
            Paragraph("<code>/at-expand &lt;prompt&gt;</code>", styles['TableCell']),
            Paragraph("Dynamically expand @context references (<code>@git:staged</code>, <code>@err:latest</code>, <code>@db:schema</code>) into prompt.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/visual-sentry</code>", styles['TableCellCode']),
            Paragraph("<code>/visual-sentry</code>", styles['TableCell']),
            Paragraph("Audit visual DOM layout for unintended CSS regressions, overlapping text, and button shifts.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/heal-watch</code>", styles['TableCellCode']),
            Paragraph("<code>/heal-watch [start | status]</code>", styles['TableCell']),
            Paragraph("Continuous autonomous background watchdog loop that repairs compiler errors as you save files.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/predict</code>", styles['TableCellCode']),
            Paragraph("<code>/predict [file] [symbol]</code>", styles['TableCell']),
            Paragraph("Anticipate next developer edits across related files before the user navigates to them.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/mode</code>", styles['TableCellCode']),
            Paragraph("<code>/mode [architect|debug|doc|sprint]</code>", styles['TableCell']),
            Paragraph("Hot-swap prompt personas and automatically harvest live external documentation for target frameworks.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/sandbox</code>", styles['TableCellCode']),
            Paragraph("<code>/sandbox &lt;stack_name&gt;</code>", styles['TableCell']),
            Paragraph("Spin up zero-config ephemeral isolated execution stack sandbox with pre-seeded database tables.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/anti-placebo</code>", styles['TableCellCode']),
            Paragraph("<code>/anti-placebo [test_file]</code>", styles['TableCell']),
            Paragraph("Run mutation testing gatekeeper to eliminate tautological and placebo unit tests.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/graduate</code>", styles['TableCellCode']),
            Paragraph("<code>/graduate [supabase|firebase|neon]</code>", styles['TableCell']),
            Paragraph("Graduate ephemeral mock APIs into real backend tables, SQL migrations, and client SDKs.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/expand</code>", styles['TableCellCode']),
            Paragraph("<code>/expand &lt;vibe_prompt&gt;</code>", styles['TableCell']),
            Paragraph("Expand casual vibe prompts into comprehensive, battle-tested engineering specifications.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/auto-heal</code>", styles['TableCellCode']),
            Paragraph("<code>/auto-heal [log_output]</code>", styles['TableCell']),
            Paragraph("Detect missing packages or crates from compiler logs and auto-install them with version locking.", styles['TableCell'])
        ],
        # Sovereign Growth & Monetization
        [
            Paragraph("<code>/saas</code>", styles['TableCellCode']),
            Paragraph("<code>/saas &lt;stripe|lemonsqueezy&gt;</code>", styles['TableCell']),
            Paragraph("Scaffold full monetization fabric: checkout sessions, webhook handlers, customer portals, and auth gates.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/voice</code>", styles['TableCellCode']),
            Paragraph("<code>/voice &lt;transcript&gt;</code>", styles['TableCell']),
            Paragraph("Full-duplex ambient conversational voice loop: speak coding commands with barge-in voice interruption.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/figma</code>", styles['TableCellCode']),
            Paragraph("<code>/figma [sync | export]</code>", styles['TableCell']),
            Paragraph("Bi-directional Figma integration: convert design tokens into Tailwind CSS and export components back to Figma.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/shadow-db</code>", styles['TableCellCode']),
            Paragraph("<code>/shadow-db [stress | analyze]</code>", styles['TableCell']),
            Paragraph("Simulate production database traffic and load-test queries to discover missing indexes and slow joins.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/viral</code>", styles['TableCellCode']),
            Paragraph("<code>/viral [generate]</code>", styles['TableCell']),
            Paragraph("Generate dynamic 1200x630 OpenGraph social preview cards and SEO metadata for high viral conversion.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/mobile</code>", styles['TableCellCode']),
            Paragraph("<code>/mobile [url]</code>", styles['TableCell']),
            Paragraph("Display terminal ANSI QR code to test your app instantly on mobile phones with PWA manifest scaffolding.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/sentry</code>", styles['TableCellCode']),
            Paragraph("<code>/sentry &lt;error_id&gt;</code>", styles['TableCell']),
            Paragraph("Ingest real-time production error payloads, reproduce failure locally, and generate auto-verified hotfix.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/gateway</code>", styles['TableCellCode']),
            Paragraph("<code>/gateway &lt;prompt&gt;</code>", styles['TableCell']),
            Paragraph("Route LLM queries through semantic cost gateway, saving 70% on cloud fees via local model arbitrage.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/funnel</code>", styles['TableCellCode']),
            Paragraph("<code>/funnel [query | scaffold]</code>", styles['TableCell']),
            Paragraph("Zero-cookie, GDPR-compliant edge analytics funnel tracking visitors from landing page to paid conversion.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/vault</code>", styles['TableCellCode']),
            Paragraph("<code>/vault [seal | audit]</code>", styles['TableCell']),
            Paragraph("Kernel-level memory-only Ghost Envs: prevent plain-text secrets from ever touching git or disk.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/typelock</code>", styles['TableCellCode']),
            Paragraph("<code>/typelock [sync]</code>", styles['TableCell']),
            Paragraph("Zero-drift polyglot type locking: keep Rust backend structs and TypeScript frontend interfaces in 100% sync.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/radar</code>", styles['TableCellCode']),
            Paragraph("<code>/radar [orbit|surface|atmo]</code>", styles['TableCell']),
            Paragraph("Spatial radar semantic zoom: view codebase structure from high-level architecture down to local AST tokens.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/cloak</code>", styles['TableCellCode']),
            Paragraph("<code>/cloak &lt;text&gt;</code>", styles['TableCell']),
            Paragraph("Zero-knowledge airgap cloaking: redact API keys, IPs, and PII before transmitting prompts to cloud LLMs.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/sqlguard</code>", styles['TableCellCode']),
            Paragraph("<code>/sqlguard &lt;query&gt;</code>", styles['TableCell']),
            Paragraph("Active SQL interceptor: prevent destructive queries (`DROP`, unindexed `DELETE`) via shadow transaction jail.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/replay</code>", styles['TableCellCode']),
            Paragraph("<code>/replay [frame_id]</code>", styles['TableCell']),
            Paragraph("Deterministic execution replay: step backwards and forwards through program execution frames mid-flight.", styles['TableCell'])
        ],
        [
            Paragraph("<code>/edge</code>", styles['TableCellCode']),
            Paragraph("<code>/edge &lt;provider&gt; &lt;slug&gt;</code>", styles['TableCell']),
            Paragraph("1-click zero-config public edge deployment to Cloudflare Workers, Vercel, Fly.io, or Vella Network.", styles['TableCell'])
        ]
    ]

    slash_table = Table(slash_commands_data, colWidths=[85, 155, 292])
    slash_table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), PRIMARY_NAVY),
        ('BOX', (0, 0), (-1, -1), 0.75, DARK_SLATE),
        ('INNERGRID', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, BG_ICE]),
        ('TOPPADDING', (0, 0), (-1, -1), 3),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 3),
        ('LEFTPADDING', (0, 0), (-1, -1), 5),
        ('RIGHTPADDING', (0, 0), (-1, -1), 5),
    ]))
    story.append(slash_table)
    story.append(Spacer(1, 6))

    story.append(make_callout(
        "SLASH COMMAND EFFICIENCY",
        "Slash commands bypass the standard natural language generation pipeline completely. When you issue `/undo`, `/mock`, "
        "or `/checkpoint`, the daemon executes the microkernel command in under 15 milliseconds, burning zero LLM tokens.",
        kind="tip",
        width=532
    ))

    return story
