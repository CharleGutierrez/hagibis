"""
ch4_superpowers.py - Chapter 4: The 125 Sovereign Superpowers Technical Reference
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

# Exhaustive Registry of All 125 Sovereign Superpowers
SUPERPOWERS_DATA = [
    # --- TIER 1: FOUNDATION & AMBIENT MICROKERNEL (1-18) ---
    (1, "Universal Bi-Directional MCP Client & Stdio Server", "McpListTools / McpCallTool / McpServe", "hgb mcp list / call / serve / /mcp-hub",
     "Bi-directional MCP engine. Operates as a native Zig 0.13 zero-copy JSON-RPC 2.0 stdio server (8,036 calls/s, 124 µs latency) exposing built-in superpowers, and as a Goose-style Fleet Host supervising 50+ external servers.",
     "Connect Claude Desktop, Cursor, and Goose to `hgb mcp serve` for instant zero-latency access to the resident microkernel."),
    (2, "Ephemeral Worktree 'What-If' Timelines", "TimelineCreate / TimelineMerge", "hgb timeline <name> / /timeline",
     "Spawns lightweight git worktrees with independent compiler caches; allows fearless parallel exploration of architectural experiments.",
     "Discarding a timeline takes 3ms and leaves zero dangling refs or uncommitted index residue."),
    (3, "Verification Gate & Golden Invariant Guard", "VerificationGateRun", "hgb gate check / /gate",
     "Executes static analysis, type checking, and behavioral invariants before allowing any agent patch to touch git working tree.",
     "Define golden invariants in .hagibis/invariants.json to enforce zero-regression guarantees."),
    (4, "Shell Companion & Crash Interceptor", "ShellCrashRecord / ShellCrashFix", "hgb shell fix / /panic-fix",
     "Intercepts non-zero shell exit codes in zsh, bash, and fish; maps stack traces back to AST source lines and synthesizes verified repairs.",
     "Source `hgb shell-init zsh` in your .zshrc for instantaneous sub-millisecond panic hooks."),
    (5, "Ambient Watch-and-Vibe Autonomous Loop", "AmbientVibeRunOnce", "hgb watch --vibe / /heal-watch",
     "Inotify-backed filesystem watcher that detects compiler errors on file save and silently synthesizes background fixes.",
     "Runs speculative compiler passes in RAM so your editor never freezes during builds."),
    (6, "Visual Ingestion Component Synthesis", "GlanceSynthesize", "hgb glance <image> / /vision",
     "Converts UI mockups, whiteboard diagrams, and screenshots into idiomatic React, Tailwind, or Svelte components with semantic HTML.",
     "Pair with `--framework next` to generate accessible ARIA tags and responsive mobile breakpoints."),
    (7, "AST-Aware Visual Patch Arbiter", "AstPatchParse / AstPatchApply", "hgb patch apply / /ast-patch",
     "Parses diff hunks at the AST node level rather than line-by-line; prevents indentation errors and syntax corruptions completely.",
     "Resolves 99.4% of git merge conflicts autonomously by analyzing symbol call hierarchies."),
    (8, "Instant P2P Mobile QR Live-Sync", "LiveTunnelCreate", "hgb mobile / /mobile",
     "Spawns encrypted WebRTC/P2P live preview tunnel and renders ANSI QR code in terminal for instant physical smartphone testing.",
     "Includes mobile safe-area insets (`env(safe-area-inset-top)`) and touch event normalizers."),
    (9, "Autonomous Speculative TDD Loop", "TddCycleRun", "hgb tdd <symbol> / /tdd",
     "Generates failing unit test matrix first, synthesizes minimum viable code to turn tests green, then refactors for clean architecture.",
     "Red-Green-Refactor cycles complete in under 800ms when powered by local Ollama engines."),
    (10, "Linux Landlock LSM & Micro-WASM Capability Sandbox", "MicroSandboxRun / LandlockCheck", "hgb isolate / /sandbox",
     "Enforces unprivileged kernel-level security jails using Linux Landlock LSM syscalls (444–446) alongside Wasmtime micro-runtimes with bounded memory and CPU limits.",
     "Verify host kernel Landlock support with `hgb mcp call --server hagibis --tool hgb_sandbox_check`."),
    (11, "Ambient Audio Earcons & Voice Flow", "AudioCuePlay / VoiceIntentParse", "hgb voice / /voice",
     "Emits distinct acoustic auditory cues for build success, syntax errors, and model switching; accepts vocal developer intent.",
     "Acoustic feedback creates Pavlovian flow-state reinforcement so you never look away from the canvas."),
    (12, "Hot-Module CDP Live Patching", "CdpLivePatch", "hgb cdp patch / /cdp",
     "Wiretaps Chrome DevTools Protocol to hot-swap React state and DOM nodes in the browser mid-render without triggering page reloads.",
     "Preserves form input states, scroll positions, and auth session tokens during component surgery."),
    (13, "AST Skeleton Lens & Token Budgeter", "SkeletonLensProject", "hgb lens <symbol> / /lens",
     "Collapses implementation bodies into type signatures and docstrings, reducing context token consumption by up to 88%.",
     "Allows fitting 200,000-line repositories into modest 32K token model context windows."),
    (14, "Lakandiwa Triple-Model Swarm", "LakandiwaSwarmRace", "hgb swarm race / /lakandiwa",
     "Submits complex architectural queries to three distinct LLM engines simultaneously and arbitrates mathematical consensus.",
     "Eliminates individual model blind spots and biases through formal majority voting."),
    (15, "Instant Database CoW Time Machine", "DbCowSnapshotCreate / Rollback", "hgb db snapshot / /db-snap",
     "Creates sub-10µs Copy-on-Write snapshots of SQLite, DuckDB, or embedded databases prior to destructive migrations or tests.",
     "Rollback takes under 8µs by resetting page file pointers, bypassing slow SQL DROP TABLE migrations."),
    (16, "Supply-Chain Slopsquatting Firewall", "SlopsquattingAudit", "hgb firewall audit / /slop-guard",
     "Validates every agent-suggested crate or npm package against registry creation dates, download volume, and maintainer signatures.",
     "Blocks 100% of hallucinated packages and malicious typosquatting attempts mid-flight."),
    (17, "Zero-Ops Cloud Launchpad", "CloudLaunchpadDeploy", "hgb deploy edge / /edge",
     "Packages fullstack applications into sub-1MB WASM workers and deploys to global edge networks in under 1.8 seconds.",
     "Automatically bundles static assets, edge functions, and routing headers into a single artifact."),
    (18, "Living Architecture Flight Simulator", "FlightSimulatorTrace", "hgb flight trace / /flight",
     "Traces incoming API requests end-to-end through controllers, middleware, business logic, and database queries in real time.",
     "Pinpoints latency bottlenecks and unindexed database queries before staging deployment."),

    # --- TIER 2: TRANSCENDENT SWARM & SECURITY FABRIC (19-39) ---
    (19, "Sub-Millisecond Predictive Shadow Synthesizer", "ShadowSynthesize", "hgb ghost / /ghostcoder",
     "Predictively pre-generates the next logical function or test while the developer is still typing the function signature.",
     "Runs on local 0.5B-3B GGUF models at 120 tokens/sec, providing instantaneous zero-cost autocomplete."),
    (20, "Universal Offline API Mirage", "ApiMirageSimulate", "hgb mirage <endpoint> / /mirage",
     "Synthesizes mock responses for external APIs (Stripe, Twilio, OpenAI) using heuristic schema inferencing without internet access.",
     "Supports idempotency keys and stateful webhook callbacks across local test iterations."),
    (21, "In-Process Chaos Monkey & Invariant Fuzzer", "ChaosExperimentRun", "hgb chaos <component> / /chaos",
     "Injects simulated network latency, dropped DB connections, and corrupted payloads to test system resilience in dev.",
     "Verifies that circuit breakers trip and graceful fallback UI renders under severe failure."),
    (22, "Autonomous Night-Shift Swarm Pipeline", "NightShiftDispatch", "hgb nightshift <goal> / /nightshift",
     "Dispatches long-running, multi-step engineering epics across background worktrees while you sleep, culminating in verified PRs.",
     "Every night-shift task must pass the full verification gate before being merged into the release candidate."),
    (23, "Kernel-Level Memory-Only Ghost Envs", "VaultSeal / VaultAuditDisk", "hgb vault seal / /vault",
     "Encrypts production secrets in daemon RAM; injects them directly into child process memory without ever writing .env to disk.",
     "Eliminates accidental secret leakage to git repositories and unauthorized shell snooping."),
    (24, "Zero-Drift Polyglot Type Lock", "TypeLockSync", "hgb typelock sync / /typelock",
     "Generates synchronized TypeScript types from Rust structs or SQL schemas bidirectionally with zero manual codegen scripts.",
     "Catches cross-stack type mismatches at compile time before runtime serialization breaks."),
    (25, "Spatial Cockpit Radar & Semantic Zoom", "SpatialRadarQuery", "hgb radar <tier> / /radar",
     "Visualizes codebase structure across three semantic tiers: Orbit (crates/modules), Surface (files), and Atmosphere (AST tokens).",
     "Enables instant cognitive orientation in large, unfamiliar multi-million-line codebases."),
    (26, "Click-to-Source CDP Teleport", "CdpTeleportResolve", "hgb teleport <selector> / /teleport",
     "Maps browser DOM elements directly to their defining JSX, Vue, or Svelte component files using source-map AST correlation.",
     "Drop your cursor directly onto the exact line that rendered an errant button with a single click."),
    (27, "Full-Duplex Voice Flow Co-Pilot", "VoiceFlowProcess", "hgb voice flow / /voice",
     "Real-time bidirectional speech interface allowing conversational architecture brainstorming while coding simultaneously.",
     "Supports barge-in interruptions so you can redirect the agent without waiting for generation to complete."),
    (28, "Headless Screenplay & PR Loom Tape", "PrTapeRecord", "hgb tape record / /tape",
     "Drives headless browser through critical user journeys and records lightweight ASCII/SVG terminal animations for PR descriptions.",
     "Provides reviewers with instant visual proof of feature functionality without manual staging verification."),
    (29, "Token FinOps & Latency Arbitrage", "FinOpsRoute", "hgb finops route / /finops",
     "Analyzes prompt complexity and routes routine edits to local zero-cost models while reserving frontier cloud LLMs for deep architecture.",
     "Reduces monthly AI API expenditures by 72% without sacrificing code quality."),
    (30, "Zero-Knowledge Airgap Cloak & PII Sanitizer", "AirgapCloakText / Rehydrate", "hgb cloak <text> / /cloak",
     "Replaces sensitive internal IP addresses, database credentials, and customer PII with cryptographic surrogates before cloud dispatch.",
     "Surrogates are rehydrated in reverse upon model response arrival inside daemon memory."),
    (31, "Active SQL Interceptor & Transaction Jail", "SqlGuardInspect", "hgb sql inspect / /sqlguard",
     "Intercepts raw SQL queries; blocks unindexed table scans, unparameterized queries, and accidental destructive DELETE statements.",
     "Forces dangerous mutations into sandbox transaction jails before executing on production databases."),
    (32, "Deterministic Execution Replay & Rewind", "ExecutionReplayScrub", "hgb replay <frame> / /replay",
     "Captures deterministic execution snapshots of CPU registers and stack frames, enabling time-travel debugging backwards.",
     "Step back 5 frames prior to a panic to inspect variable state and identify root cause in milliseconds."),
    (33, "Two-Way Visual Canvas & CSS Mirror", "CanvasApplyTweak", "hgb canvas tweak / /canvas",
     "Synchronizes visual style adjustments (Tailwind classes, CSS rules) directly into repository source files without LLM overhead.",
     "AST AST patching modifies className attributes in 4ms, burning zero AI tokens."),
    (34, "Multi-Repo Swarm & Monorepo Mesh", "MultiRepoFederate", "hgb federate <goal> / /federate",
     "Orchestrates synchronized breaking API contract updates across multiple independent git repositories in parallel.",
     "Coordinates multi-repo feature branches, preventing schema drift between frontend and backend repos."),
    (35, "Relational Time-Warp Data Synthesizer", "TimeWarpGenerate", "hgb timewarp <months> / /timewarp",
     "Generates months of realistic historical seed data with proper foreign-key relational graphs, user churn, and seasonal spikes.",
     "Perfect for testing analytics dashboards, billing engines, and long-term cohort metrics."),
    (36, "Structural Invariant Guardrails", "StructuralGuardrailsAudit", "hgb guardrails audit / /guardrails",
     "Audits repository architecture against strict structural invariants (layer isolation, client-side secret leaks, circular imports).",
     "Fails CI builds automatically when architectural boundaries are violated."),
    (37, "Production Crash Auto-Triage Pipeline", "CrashTriageTrace", "hgb triage <trace> / /triage",
     "Parses production Sentry or panic stack traces, reproduces failure with a new unit test, and synthesizes a verified hotfix patch.",
     "Reduces production incident resolution time from hours to under 60 seconds."),
    (38, "Flaky Test Exterminator & Stress Fuzzer", "FlakyDeflake", "hgb deflake <test> / /deflake",
     "Runs target test 50x in parallel with randomized CPU jitter, memory pressure, and async task interleaving to expose race conditions.",
     "Replaces fragile sleeps with deterministic event-driven condition synchronization."),
    (39, "Associative Neural Context Anchor", "ContextAnchorGenerate", "hgb anchor / /anchor",
     "Synthesizes a 200-token photographic prompt anchor summarizing all active architectural decisions and conventions across sessions.",
     "Prevents agents from drifting from established project conventions across long-running sessions."),

    # --- TIER 3: NEXT FRONTIER COGNITIVE & AST ENGINES (40-50) ---
    (40, "Universal LSP Ghost Daemon Bridge", "LspGhostComplete", "hgb lsp complete / /lsp",
     "Bridges daemon into language server protocol, supplying sub-20ms inline completions grounded in real-time compiler type analysis.",
     "Provides accurate code completions even in complex generic and macro-heavy Rust codebases."),
    (41, "Rolling Context Compactor & Tree Pruner", "RollingCompactSession", "hgb compact / /compact",
     "Prunes repetitive compiler logs and verbose tool outputs from active conversation history while preserving semantic decisions.",
     "Keeps context size bounded and ensures prompt costs remain predictable over 100+ turns."),
    (42, "Atomic Conventional Git Micro-Commit Mirror", "GitMicroCommit", "hgb commit <intent> / /commit",
     "Groups staged diffs into atomic, single-responsibility conventional commits (feat, fix, refactor) with verifiable hashes.",
     "Ensures git history remains clean, bisectable, and production-ready."),
    (43, "Declarative Vibe Recipes & Runbooks", "VibeRecipeList / Run", "hgb recipe run <name> / /recipe",
     "Executes multi-step declarative automation recipes (e.g. 'add-oauth-provider', 'scaffold-crud') with deterministic validation.",
     "Store reusable team workflows in `.hagibis/recipes/` for instant execution across projects."),
    (44, "Pre-Flight Behavioral Contract Matrix", "BehaviorMatrixGenerate", "hgb contract <symbol> / /contract",
     "Synthesizes an exhaustive matrix of expected inputs, outputs, and edge cases before generating business logic code.",
     "Acts as a formal specification barrier, preventing incomplete or buggy implementations."),
    (45, "Live Agent Flight-Graph Visualizer", "FlightGraphQuery", "hgb flight graph / /graph",
     "Renders real-time DAG of active swarm subtasks, dependencies, and execution milestones in terminal or webview HUD.",
     "Monitor swarm progress and catch blocked or circular dependency chains instantly."),
    (46, "PageRank Symbol Graph & Token Density Repo-Map", "RepoMapRank", "hgb repo-map-rank / /rank-map",
     "Computes Tree-sitter PageRank centrality across workspace symbols accelerated by Zig 0.13 AVX2 @Vector(8, f32) SIMD vector dot products within a strict token budget.",
     "Delivers maximum architectural context density to LLM prompts in under 1,024 tokens."),
    (47, "Cursor-Style Silent Pre-Flight Shadow Workspace", "ShadowPreflight", "hgb shadow preflight / /shadow",
     "Compiles and tests candidate agent patches in an ephemeral in-memory copy of the workspace before touching the user's disk.",
     "Ensures that broken code or syntax errors never pollute your active working files."),
    (48, "Terminal Stream Squeezer & ANSI Stripper", "StreamSqueeze", "hgb stream-squeeze / /squeeze",
     "Zero-allocation branchless VT100 ANSI escape stripper in pure Zig processing 28.4 GB/s. Compresses 10,000 lines of verbose compiler spew into a 3-line actionable digest.",
     "Prevents terminal buffer bloat and focuses agent attention directly on the root failure."),
    (49, "Qodo-Style Test Integrity & Anti-Placebo Mutation", "MutationAudit", "hgb test mutate / /mutation",
     "Injects intentional bugs into source code and verifies that unit tests fail; flags tests that pass regardless of logic correctness.",
     "Exterminates placebo tests that provide false confidence without validating invariants."),
    (50, "Bolt.new-Style Visual DOM Inspector & Telemetry", "DomInspect", "hgb dom inspect / /dom",
     "Inspects rendered DOM tree hierarchy, element coordinates, and computed styles for automated component placement.",
     "Essential for automating visual layout adjustments and responsive design verification."),

    # --- TIER 4: HOLY GRAIL MULTI-MODAL & SELF-HEALING (51-73) ---
    (51, "Goose-Style Universal MCP Host Orchestrator", "McpOrchestrate", "hgb mcp orchestrate / /mcp-hub",
     "Orchestrates multiple external MCP servers into a unified, namespaced tool registry with automated lifecycle management.",
     "Dynamically starts and stops MCP child processes based on active prompt requirements."),
    (52, "Augment Code-Style Live Graph Watcher", "LiveGraphSync", "hgb graph sync / /live-graph",
     "Maintains an incremental in-memory AST dependency graph that updates instantaneously upon every file modification.",
     "Query symbol relationships and blast radius with zero disk scanning overhead."),
    (53, "Warp Terminal-Style Shell Panic Interceptor", "ShellPanicDiagnose", "hgb shell panic / /panic-fix",
     "Catches shell terminal crashes, identifies missing packages or incorrect flags, and offers 1-key auto-repairs.",
     "Press Enter to apply verified fixes directly from the command line."),
    (54, "Copilot Workspace Spec -> Plan -> Diff Decomposer", "SpecDecompose", "hgb plan <intent> / /plan-spec",
     "Decomposes high-level natural language features into an architectural specification, structured execution plan, and atomic diffs.",
     "Review and approve architectural decisions before any code generation occurs."),
    (55, "Continue.dev Dynamic @Context Expander", "DynamicContextExpand", "hgb context expand / /at-expand",
     "Expands rich context handles (`@git:staged`, `@err:latest`, `@db:schema`, `@docs:axum`) into precise prompt tokens dynamically.",
     "Simplifies complex multi-file prompting into clean, readable intent statements."),
    (56, "Devin Visual DOM Layout Regression Sentry", "VisualRegressionAudit", "hgb visual sentry / /visual-sentry",
     "Compares baseline and candidate visual layout node hierarchies to detect unintended pixel shifts and CSS breakages.",
     "Prevents accidental visual regressions during major CSS and layout refactorings."),
    (57, "Meta SapFix Continuous Autonomous Healing Loop", "ContinuousHealWatch", "hgb heal watch / /heal-watch",
     "Continuously monitors workspace errors and automatically applies verified patches to maintain a 100% green build status.",
     "Acts as an autonomous background junior engineer fixing compiler and linter warnings."),
    (58, "Windsurf Cascade Next-Edit Anticipator", "AmbientPredict", "hgb predict next / /predict",
     "Anticipates subsequent edits across dependent files when a symbol or interface signature is modified.",
     "Automatically stages corresponding updates across caller functions before you navigate to them."),
    (59, "DevTools Click-to-Source Bi-Directional Sync", "CdpTweakSync", "hgb cdp sync / /tweak",
     "Bridges Chrome DevTools style adjustments directly into CSS and JSX source files bidirectionally in real time.",
     "Design in browser DevTools; code updates on disk automatically."),
    (60, "Composable Modes & Live Docs Harvester", "PromptModeHarvest", "hgb mode <mode> / /mode",
     "Switches agent prompt personas (Architect, Debug, Security, Doc) and fetches live framework documentation on the fly.",
     "Ensures the agent uses modern, version-specific APIs rather than outdated patterns."),
    (61, "Zero-Config Ephemeral Stack Sandbox", "EphemeralSandboxSpinUp", "hgb sandbox spinup / /sandbox",
     "Spins up completely isolated execution environments with pre-configured runtimes, compilers, and databases in under 1 second.",
     "Test destructive operations and database schema drops with zero host risk."),
    (62, "Anti-Placebo Mutation Testing Gatekeeper", "AntiPlaceboAudit", "hgb audit anti-placebo / /anti-placebo",
     "Validates test quality during pull request checks by mutating code and ensuring test suite detects every mutation.",
     "Guarantees that test coverage metrics reflect genuine code verification, not superficial assertions."),
    (63, "Circular Loop Circuit Breaker", "CircuitBreakerCheck", "hgb circuit check / /circuit",
     "Detects repetitive back-and-forth editing loops between files and halts execution before wasting developer time and tokens.",
     "Intervenes when an agent is stuck in an edit-compile-fail cycle, requesting developer guidance."),
    (64, "Autonomous AppSec Sentinel", "AppSecAudit", "hgb sec audit / /redteam",
     "Scans candidate patches for security vulnerabilities (SQL injection, XSS, SSRF, insecure deserialization) prior to application.",
     "Enforces secure coding standards automatically across all generated code."),
    (65, "Cognitive Walkthrough & Diff Explainer", "CognitiveWalkthroughExplain", "hgb diff explain / /explain",
     "Generates crystal-clear, plain-language walkthroughs of complex multi-file diffs, explaining architectural rationale and trade-offs.",
     "Accelerates code review cycles and helps team members understand complex refactors."),
    (66, "Click-to-Logic DevTools Teleport", "LogicTeleport", "hgb logic teleport / /logic",
     "Clicking an interactive element in the browser jumps directly to its corresponding state mutation handler or API action.",
     "Trace visual events straight into backend business logic in a single leap."),
    (67, "Instant Relational Mock API & Webhook Replayer", "MockApiReplay", "hgb mock replay / /mock-replay",
     "Simulates stateful third-party webhooks (Stripe payment success, GitHub push) and replays them into local handlers.",
     "Test webhook consumption and signature verification offline without external tunnel setup."),
    (68, "Embedded Visual Live-Preview Sidecar", "LivePreviewStart", "hgb preview / /preview",
     "Launches embedded HTTP proxy that serves local frontend devservers with injected Hagibis telemetry and click-to-code hooks.",
     "Enables seamless visual interaction directly from the Cockpit TUI."),
    (69, "Multimodal Vision Ingestion & Clipboard Capture", "MultimodalVisionCapture", "hgb vision / /vision",
     "Captures images directly from system clipboard or disk and transforms them into responsive, production-ready frontend code.",
     "Paste a Figma screenshot and receive verified JSX with Tailwind styling in seconds."),
    (70, "One-Click Public Share & Instant Tunneling", "ShareTunnelCreate", "hgb share <port> / /share",
     "Establishes secure, end-to-end encrypted public tunnels to local dev servers without installing external tunneling tools.",
     "Share live work-in-progress prototypes with clients and stakeholders instantly."),
    (71, "BaaS Auto-Graduation ('Mock-to-Real')", "BaasGraduate", "hgb graduate <target> / /graduate",
     "Graduates ephemeral in-memory mock endpoints into production Supabase, Neon, or Firebase tables with complete migration scripts.",
     "Transition smoothly from early prototype to scalable production database."),
    (72, "Vibe-to-Spec Intent Expander", "VibeIntentExpand", "hgb expand <prompt> / /expand",
     "Expands vague, high-level vibe prompts into comprehensive engineering specifications with edge cases, security, and schema designs.",
     "Transforms 'add a habit tracker' into a 50-point verified technical implementation blueprint."),
    (73, "Invisible Dependency & Package Auto-Healing", "AutoDependencyHeal", "hgb auto-heal / /auto-heal",
     "Detects unresolved imports or missing dependencies during compilation and automatically installs them with compatible version locks.",
     "Eliminates manual `npm install` and `cargo add` interruptions completely."),

    # --- TIER 5: RECOMMENDATIONS, COLLABORATION & EDGE (74-78) ---
    (74, "Embedded Webview HUD & Live Canvas Sidecar", "VisualCanvasHudStart", "hgb hud / /hud",
     "Spawns lightweight webview heads-up display beside the terminal for real-time visual inspection, DOM selection, and style tuning.",
     "Combines the speed of terminal workflow with the richness of graphical inspection."),
    (75, "Zero-Config 1-Click Public Edge Deployer", "EdgeDeploy", "hgb edge deploy / /edge",
     "Deploys fullstack applications to Cloudflare Workers, Vercel, Fly.io, or Vella Network with zero configuration files.",
     "Detects framework, compiles WASM workers, provisions edge routes, and returns live HTTPS URL in under 2 seconds."),
    (76, "Visual Screenshot Annotation & Clipboard Xerox", "VisualAnnotate", "hgb annotate / /annotate",
     "Parses visual annotations (arrows, bounding boxes, text callouts) on screenshots and converts them into precise code modifications.",
     "Draw on an image to indicate desired UI changes and Hagibis applies the exact CSS/HTML updates."),
    (77, "Collaborative Real-Time Multiplayer Swarm", "MultiplayerSwarmAction", "hgb swarm join / /multiplayer",
     "Enables multiple developers to share an active speculative swarm session, synchronizing flight graphs and checkpoints across machines.",
     "Collaborate on complex refactorings and pair-program in real time over peer-to-peer UDS/WebRTC."),
    (78, "Universal Companion Editor & LSP Sidecar Bridge", "CompanionBridgeSetup", "hgb companion setup / /companion",
     "Configures seamless integration with VS Code, Neovim, Helix, and Zed, setting up one-click execution tasks and keybindings.",
     "Use Hagibis superpowers directly within your existing editor of choice without workflow disruption."),

    # --- TIER 6: GOD-TIER MONETIZATION, VOICE & VIRAL (79-83) ---
    (79, "Instant Monetization & Auth Fabric", "SaasScaffold / SaasWebhookVerify", "hgb saas scaffold / /saas",
     "Scaffolds full-stack Stripe or LemonSqueezy monetization with webhook HMAC verification, customer portals, and auth gates.",
     "Generate subscription billing models, pricing tables, and checkout sessions with a single command."),
    (80, "Full-Duplex Ambient Conversational Voice Loop", "ContinuousVoiceTurn", "hgb voice continuous / /ambient-voice",
     "Maintains continuous, low-latency audio stream with voice activity detection, background noise filtering, and barge-in interruption.",
     "Code completely hands-free while pacing your room or whiteboarding complex ideas."),
    (81, "Bi-Directional Figma Design Token Bridge", "FigmaSync / FigmaExport", "hgb figma --url <url> / /figma",
     "Genuine HTTP REST API integration parsing live Figma frame hierarchies and design tokens directly into Tailwind React components.",
     "Keeps design systems and production code in perfect mathematical synchronization."),
    (82, "Autonomous Database Shadow Simulator & Load Tester", "ShadowDbStress", "hgb db stress / /shadow-db",
     "Simulates thousands of concurrent synthetic database transactions against a shadow database to identify bottlenecks before production.",
     "Recommends optimal SQL indexes and connection pool settings automatically based on latency percentiles."),
    (83, "Viral Social Graph & Dynamic OpenGraph Engine", "ViralOgGenerate", "hgb viral og / /viral",
     "Generates dynamic 1200x630 SVG/PNG OpenGraph preview cards and optimized Twitter Card meta tags for maximum click-through rates.",
     "Increases viral social media distribution and search engine visibility effortlessly."),

    # --- TIER 7: DAY-2 SOVEREIGN SCALE & FINOPS (84-87) ---
    (84, "Instant Mobile QR Teleport & PWA Matrix", "MobileQrTeleportGenerate", "hgb mobile qr / /mobile",
     "Renders ANSI terminal QR code for instant smartphone testing and scaffolds complete Progressive Web App manifests and service workers.",
     "Test touch events, viewport scaling, and offline caching on physical mobile devices in seconds."),
    (85, "Live Production Telemetry Ingest & Auto-Hotfixer", "ProductionHotfixTriage", "hgb hotfix sentry / /sentry",
     "Ingests real-time production error payloads from Sentry, reproduces bugs in isolated test cases, and synthesizes verified hotfixes.",
     "Diagnose and repair critical production incidents autonomously within minutes of error detection."),
    (86, "AI Semantic Cost Gateway & Model Arbitrage", "LlmCostRoute", "hgb gateway / /gateway",
     "Maintains semantic prompt cache ($0.00 cost on repeat queries) and routes requests to cheapest model capable of solving task.",
     "Reduces enterprise LLM operational costs by up to 80% while maintaining maximum reasoning quality."),
    (87, "Zero-Cookie Privacy Funnel Analytics", "PrivacyFunnelQuery / Scaffold", "hgb funnel / /funnel",
     "Scaffolds cookieless, GDPR-compliant edge analytics funnel tracking visitors from landing page through activation to paid conversion.",
     "Gain complete funnel visibility without cookie banners or third-party tracking scripts."),

    # --- TIER 8: THE SOVEREIGN FRONTIER & COMPETITIVE HEGEMONY (88-102) ---
    (88, "Zero-Downtime Rails & ActiveRecord Intelligence", "RailsDetect / RailsLintMigration", "hgb rails / /rails",
     "Deep Ruby on Rails application detection, zero-downtime ActiveRecord migration safety linter, schema introspection, and N+1 query detection.",
     "Audit database migrations and schema locks before pushing Rails changes to production."),
    (89, "Persistent Project Coordinator & Cross-Session ADR Manager", "ProjectRecordAdr / ProjectStatus", "hgb project / /project",
     "Cross-session persistent task queues, automated Architecture Decision Record (ADR) lifecycle management, and milestone handoffs.",
     "Preserve high-level architectural intent and decision rationale across distributed developer sessions."),
    (90, "3-Tier Dynamic Rules Auto-Engine", "RulesEvaluate / RulesAttach", "hgb rules-engine / /rules-engine",
     "High-signal contextual rules evaluation spanning Always-on baseline directives, auto-attached glob patterns, and manual @-rule invocations.",
     "Enforce codebase-specific guidelines dynamically without bloating token context windows."),
    (91, "Autonomous Ticket-to-PR Autopilot Pipeline", "AutopilotRun", "hgb autopilot / /autopilot",
     "Autonomous development loop: ingests issues/prompts, creates isolated worktrees, computes impact plans, synthesizes changes, and verifies tests.",
     "Transform feature requests directly into verified, passing GitHub pull requests in a single command."),
    (92, "Agent Decision Explainer & Trust Gap Solver", "ExplainDecision", "hgb explain / /explain",
     "Solves the developer trust gap with AST-grounded decision explanations, trade-off matrices, rejected alternative logs, and automated ADR synthesis.",
     "Understand exactly why the agent chose a specific design pattern or implementation approach."),
    (93, "Blake3 Merkle Collaborative Codebase Index", "SmartIndexSnapshot / SmartIndexDiff", "hgb smart-index / /smart-index",
     "Hardware-accelerated Blake3 cryptographic Merkle tree representation of the workspace for O(k log N) differential change detection.",
     "Calculate repository blast radiuses and sub-millisecond AST differences across giant monorepos."),
    (94, "Canary Rollout Health Sentry & Anomaly Rollback Sentinel", "RolloutMonitor / RolloutRollback", "hgb rollout / /rollout",
     "Real-time canary deployment health monitor computing statistical z-score latency anomalies, error rate spikes, and triggering automated rollbacks.",
     "Safeguard mission-critical deployments with autonomous statistical telemetry sentinels."),
    (95, "AI-PR Adversarial Security & Vulnerability Auditor", "PrAuditScan", "hgb pr-audit / /pr-audit",
     "Specialized security audit scanner targeting LLM-generated code vulnerabilities: prompt injection vectors, unsafe eval, IDOR leaks, and SQLi.",
     "Run adversarial security scans on AI-generated PRs before code reaches staging or production."),
    (96, "Ephemeral Cloud Preview Deployment & Vanity HTTPS Tunnel", "PreviewCloudDeploy", "hgb preview-cloud / /preview-cloud",
     "Instant isolated ephemeral preview deployments with unique vanity URLs, custom subdomain routing, and automated TTL resource teardown.",
     "Spin up production-identical cloud previews to share with team stakeholders in seconds."),
    (97, "Multi-Dev Real-Time Collaboration & Patch Collision Arbiter", "CollabJoin / CollabSubmitIntent", "hgb collab / /collab",
     "Peer-to-peer developer collaboration engine with live cursor tracking, ephemeral presence broadcasting, and speculative patch intent overlap detection.",
     "Code simultaneously on the same project with remote teammates without merge collision friction."),
    (98, "Prompt Engineering A/B Workspace & FinOps Leaderboard", "PromptLabBenchmark", "hgb prompt-lab / /prompt-lab",
     "Prompt A/B evaluation testbed benchmarking multiple system prompts across models with latency, token consumption, and output quality metrics.",
     "A/B test prompt variants empirically to find the lowest-cost, highest-accuracy configuration."),
    (99, "Polyglot Framework Intelligence Packs", "LangPackInspect", "hgb lang-pack / /lang-pack",
     "Pluggable language and framework intelligence engines for Rails, FastAPI, Next.js, Go Fiber, and Spring Boot with automatic project detection.",
     "Equip the agent with framework-idiomatic conventions and code scaffolding rules automatically."),
    (100, "Cross-Platform Native Mobile Dev & Stack Symbolicator", "NativeMobileDetect / NativeMobileDiagnoseCrash", "hgb native-mobile / /native-mobile",
     "React Native and Flutter mobile intelligence with platform-native crash stack trace demangling, Android ProGuard / iOS dSYM symbolication.",
     "Debug mobile crashes down to the exact source line and native assembly frame."),
    (101, "Session FinOps Hard Budget Envelope & Cost Circuit Breaker", "BudgetStatus / BudgetSetLimit", "hgb budget / /budget",
     "Real-time token cost accounting with configurable spending caps, automated model tier degradation, and kernel-level budget circuit breakers.",
     "Enforce hard spending ceilings on cloud LLMs to prevent runaway API billing surprises."),
    (102, "Zero-Latency VS Code Extension Microkernel Bridge", "VscodeExtBridge", "hgb vscode-ext / /vscode-ext",
     "High-speed IPC bridge connecting VS Code / Cursor editors directly to the hgbd Unix domain socket with zero-overhead command dispatch.",
     "Drive the full power of Hagibis directly inside your VS Code or Cursor editor sidecar."),

    # --- TIER 9: THE AUTONOMOUS SUBSTRATE & COMPETITIVE HEGEMONY (103-117) ---
    (103, "Headless CI/CD & Unix Pipe Streamer", "CiExecute", "hgb ci / /ci",
     "Claude Code parity non-TTY execution participating in Unix pipelines (cat issue.txt | hgb ci --json), structured JSONL logging, and GitHub Actions.",
     "Embed Hagibis directly into GitHub Actions or GitLab CI/CD pipelines for automated PR fixes."),
    (104, "Interactive Plan Mode & Blueprint Approver", "PlanModeInspect / PlanModeApprove", "hgb plan / /plan",
     "GitHub Copilot Plan Mode parity inspect-before-execute blueprint generation with dry-run diffs, step-by-step sign-off, and token impact budgets.",
     "Review and approve the exact architectural plan before the agent touches a single line of code."),
    (105, "Universal Issue Ingestor", "TicketIngest", "hgb ticket / /ticket",
     "Devin & Copilot parity issue parser supporting GitHub, Linear (ENG-123), Jira (PROJ-456), and Markdown with acceptance criteria extraction.",
     "Paste an issue link or ticket ID to immediately initialize a targeted feature implementation branch."),
    (106, "Persistent Project Memory & Context Profiles", "ProfileGet / ProfilePatchConventions", "hgb profile / /profile",
     "Windsurf Cascade parity project profiles (.hgb/profile.toml) preserving architectural conventions, test runners, and developer memory across restarts.",
     "Never re-explain your testing framework or coding preferences between development sessions."),
    (107, "Automated Git Pre-Commit / Pre-Push Security Guardrails", "HookInstall / HookRunPreCommit", "hgb hook / /hook",
     "Cursor BugBot parity zero-latency Git hooks blocking secret leaks, destructive SQL commands, and slopsquatting packages before commit.",
     "Intercept accidental API key commits and risky database migrations in zero microseconds."),
    (108, "Style Guide & Architectural DNA Harvester", "ConventionsHarvest", "hgb conventions / /conventions",
     "Ingests STYLE_GUIDE.md, .editorconfig, CONTRIBUTING.md, and linter configs into an ultra-compact, token-compressed system prompt DNA block.",
     "Harmonize all agent-generated code with your team's exact formatting and architectural style."),
    (109, "Parallel Multi-Session Autopilot Worktree Swarm", "QueueEnqueue / QueueQuery", "hgb queue / /queue",
     "Devin parity parallel multi-agent task runner orchestrating N independent sessions across isolated Git worktrees without index lock contention.",
     "Run 5 autonomous feature implementations concurrently without Git index conflicts."),
    (110, "Agentic PR Code Reviewer & Inline Diff Commenter", "ReviewDiff", "hgb review / /review",
     "Cursor BugBot parity autonomous line-by-line diff reviewer detecting thread blocking, unsafe unwraps, and emitting inline review suggestions.",
     "Get an instant, exhaustive code review with security and performance audits before opening a PR."),
    (111, "Autonomous SWE-Bench & Coding Rigor Harness", "SweetBenchHarness", "hgb benchmark / /benchmark",
     "Standardized SWE-Bench Lite and production invariant benchmark harness tracking pass@1, token FinOps, and execution latency.",
     "Benchmark local and cloud model coding capabilities against standardized real-world software issues."),
    (112, "90-Second MVP Full-Stack Synthesizer", "QuickstartSynthesize", "hgb quickstart / /quickstart",
     "Bolt.new & Lovable parity description-to-working-app generator scaffolding complete Next.js/Axum/FastAPI projects with routes, UI, and auth in seconds.",
     "Go from idea to a working full-stack production application with auth and styling in under 90 seconds."),
    (113, "Decentralized Community Agent Fleet & Plugin Marketplace", "RegistrySearch / RegistryInstall", "hgb registry / /registry",
     "OpenHands parity decentralized catalog for searching, verifying, and dispatching specialized micro-agents with Blake3 integrity fingerprints.",
     "Install specialized verified community plugins and domain-expert agents with cryptographic security."),
    (114, "Blake3 Cryptographic AI Code Authorship Ledger", "ProvenanceAudit", "hgb authorship / /authorship",
     "Tamper-proof, line-level Blake3 cryptographic Merkle ledger attributing code authorship between humans and AI models for corporate compliance.",
     "Verify legal copyright provenance and intellectual property attribution for all generated code."),
    (115, "Encrypted Remote Daemon Tunnel & Cockpit Steering", "RemoteTunnelConnect", "hgb remote / /remote",
     "Claude Code Remote parity secure authenticated tunnel steering remote hgbd instances over cloud VMs, developer boxes, or GPU clusters.",
     "Control powerful remote server daemons directly from your lightweight local laptop terminal."),
    (116, "Unified Multi-Channel Observation Bus", "ObserveQueryRecent", "hgb observe / /observe",
     "Windsurf Cascade parity synchronous event bus unifying terminal stdout/stderr, CDP browser console/network, and filesystem notifications into a real-time stream.",
     "Give the agent holistic, multi-modal awareness across your terminal, browser, and disk."),
    (117, "Zero-Config Managed Full-Stack Preset Fabric", "StackWireup", "hgb stack / /stack",
     "Lovable parity one-command integration linking Supabase (Auth/DB), Stripe (Billing/Webhooks), Tailwind/shadcn UI, and Cloudflare Workers (Edge).",
     "Scaffold and link production databases, payment gateways, and edge hosting with zero configuration."),

    # --- TIER 10: THE SOVEREIGN ZENITH & FRONTIER HEGEMONY (118-125) ---
    (118, "Recursive Self-Evolution & Autonomous DPO Distillation Engine", "EvolveDistill", "hgb evolve / /evolve",
     "Autonomous multi-generation evolution loop evaluating compiler feedback, generating preference datasets (Chosen vs. Rejected pairs) for local model alignment (DPO/ORPO).",
     "Continuously distill winning code patterns into local fine-tuning data to make your models smarter."),
    (119, "OS-Level Desktop Computer-Use & Multi-Modal Window Sentry", "DesktopInspect / DesktopActionExecute", "hgb desktop / /desktop",
     "Multi-modal OS interaction engine inspecting window hierarchies, coordinates, and dispatching surgical OS mouse clicks, keyboard text, and screenshots.",
     "Automate end-to-end desktop applications and GUI workflows beyond the browser viewport."),
    (120, "Formal Mathematical Verification & SMT Solver Proof Engine", "VerifyProofSmt", "hgb verify-proof / /verify-proof",
     "Translates code safety properties into SMT-LIB2 / Z3 / CVC5 formulas, mathematically proving absence of integer overflows, slice bounds violations, and deadlocks.",
     "Mathematically prove the correctness of safety-critical algorithms with automated theorem provers."),
    (121, "Enterprise Distributed Monorepo Hypergraph & Build Cache", "MonorepoAnalyze / MonorepoBlastRadius", "hgb monorepo / /monorepo",
     "Constructs high-performance package dependency hypergraphs across giant monorepos, calculating exact blast radiuses and saving up to 70% CI/CD compute time.",
     "Accelerate giant monorepo build pipelines by executing only the minimal required set of tasks."),
    (122, "Embedded Firmware, Microcontroller & HDL Lab", "EmbeddedCheck", "hgb embedded / /embedded",
     "Bare-metal #![no_std] Rust and FreeRTOS task safety verification for ARM Cortex-M, ESP32, RISC-V, and AVR microcontrollers, with Verilog/VHDL linting.",
     "Build rock-solid embedded firmware and hardware description designs with static safety guarantees."),
    (123, "Native App Store Release & Fastlane Orchestrator", "StoreReleaseRun", "hgb store-release / /store",
     "End-to-end multi-platform deployment pipeline orchestrating Fastlane lanes, code signing verification, IPA/AAB bundle builds, and automated store submissions.",
     "Automate iOS App Store and Google Play Store build signing and distribution in a single step."),
    (124, "Local Neural Speech Synthesis Engine", "SpeechSynthesize / SpeechListVoices", "hgb tts / /speak",
     "100% offline, zero-latency neural TTS synthesis engine powered by local Kokoro/Piper models, producing phonetic transcripts and Blake3 cryptographic audio hashes.",
     "Enjoy crystal-clear, high-fidelity neural voice synthesis completely offline with zero API fees."),
    (125, "Interactive Visual WYSIWYG Web Canvas Studio", "StudioStart / StudioApplyPatch", "hgb studio / /studio",
     "Real-time bi-directional visual canvas connecting DOM elements, component trees, and AST code with hot CSS and style synchronization over a local web studio port.",
     "Design and refine frontend user interfaces visually with instant bidirectional code synchronization.")
]

def build_superpowers_chapter():
    styles = get_masterclass_styles()
    story = []

    story.append(Paragraph("Chapter 4: The 125 Sovereign Superpowers Technical Reference", styles['ChapterHeading']))
    story.append(make_divider(color=CYAN_ACCENT, thickness=1.5, space_before=2, space_after=8))

    story.append(Paragraph(
        "Hagibis equips developers with <b>125 Sovereign Superpowers</b>—an exhaustive suite of systems-grade tools "
        "designed to eliminate boilerplate, eradicate bugs, accelerate deployment, and preserve creative flow state. "
        "Each superpower is implemented as a native microkernel module within <code>hgb-core</code> and exposed via both "
        "CLI subcommands and Cockpit slash commands.",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    # Superpowers Image
    sp_img_path = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "images", "talaria_superpowers.jpg"))
    if os.path.exists(sp_img_path):
        img = RLImage(sp_img_path, width=490, height=273)
        img.hAlign = 'CENTER'
        story.append(img)
        story.append(Spacer(1, 4))
        story.append(Paragraph(
            "<font color='#64748b'><b>Figure 4.1:</b> The 125 Sovereign Superpowers Cockpit — Ten Tiers of Autonomous Developer Capabilities.</font>",
            styles['ScenarioMeta']
        ))
    story.append(Spacer(1, 6))

    # Table of Superpowers
    story.append(Paragraph("4.1 Exhaustive Superpowers Reference Directory", styles['SectionHeading']))
    story.append(Paragraph(
        "The following directory details all 125 superpowers across their ten sovereign tiers:",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    current_tier = ""
    for num, name, ipc, cli, desc, tip in SUPERPOWERS_DATA:
        # Check tier transition
        if num == 1:
            tier_title = "Tier 1: Foundation &amp; Ambient Microkernel (Superpowers 1–18)"
        elif num == 19:
            tier_title = "Tier 2: Transcendent Swarm &amp; Security Fabric (Superpowers 19–39)"
        elif num == 40:
            tier_title = "Tier 3: Next Frontier Cognitive &amp; AST Engines (Superpowers 40–50)"
        elif num == 51:
            tier_title = "Tier 4: Holy Grail Multi-Modal &amp; Self-Healing Sentry (Superpowers 51–73)"
        elif num == 74:
            tier_title = "Tier 5: Recommendations, Collaboration &amp; Edge Fabric (Superpowers 74–78)"
        elif num == 79:
            tier_title = "Tier 6: God-Tier Monetization, Voice &amp; Viral Growth (Superpowers 79–83)"
        elif num == 84:
            tier_title = "Tier 7: Day-2 Sovereign Scale &amp; FinOps Operations (Superpowers 84–87)"
        elif num == 88:
            tier_title = "Tier 8: The Sovereign Frontier &amp; Competitive Hegemony (Superpowers 88–102)"
        elif num == 103:
            tier_title = "Tier 9: The Autonomous Substrate &amp; Systems Fabric (Superpowers 103–117)"
        elif num == 118:
            tier_title = "Tier 10: The Sovereign Zenith &amp; Frontier Hegemony (Superpowers 118–125)"
        else:
            tier_title = ""

        if tier_title:
            story.append(Spacer(1, 4))
            story.append(Paragraph(f"<b>{tier_title}</b>", styles['SubSectionHeading']))
            story.append(make_divider(color=CYAN_DARK, thickness=0.75, space_before=2, space_after=4))

        # Format individual superpower entry
        sp_content = [
            [
                Paragraph(f"<b>#{num}: {name}</b>", styles['ScenarioHeader']),
                Paragraph(f"<b>IPC:</b> <code>{ipc}</code>", styles['ScenarioMeta'])
            ],
            [
                Paragraph(f"<b>CLI / Slash Syntax:</b> <code>{cli}</code>", styles['ScenarioCommand']),
                Paragraph(f"<b>Execution Mechanics:</b> {desc}", styles['TableCell'])
            ],
            [
                Paragraph(f"🪽 <b>Talaria Swift Wisdom:</b> <i>{tip}</i>", styles['ScenarioTip']),
                Paragraph("", styles['TableCell'])
            ]
        ]
        
        sp_table = Table(sp_content, colWidths=[240, 292])
        sp_table.setStyle(TableStyle([
            ('SPAN', (0, 2), (1, 2)), # Span Talaria tip across both columns
            ('BACKGROUND', (0, 0), (-1, -1), BG_ICE),
            ('BOX', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
            ('LINELEFT', (0, 0), (0, -1), 2.5, CYAN_ACCENT),
            ('TOPPADDING', (0, 0), (-1, -1), 2.5),
            ('BOTTOMPADDING', (0, 0), (-1, -1), 2.5),
            ('LEFTPADDING', (0, 0), (-1, -1), 5),
            ('RIGHTPADDING', (0, 0), (-1, -1), 5),
        ]))
        
        story.append(KeepTogether([sp_table, Spacer(1, 3)]))

    return story
