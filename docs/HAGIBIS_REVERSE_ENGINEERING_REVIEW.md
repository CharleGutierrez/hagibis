**Hagibis: reverse-engineering review and improvement plan**

Historical snapshot: the code has changed since this review. See the [25 October 2026 analysis](HAGIBIS_ANALYSIS_2026-10-25.md) for current findings, completed improvements, and remaining gaps.

Reviewed on 24 October 2026. This review follows the local source code, manifests, command routing, storage, build configuration, and selected tests. It is a structural review of the main execution paths, not an exhaustive audit of every feature or a validation of live external integrations.

Hagibis is an AI-assisted development toolkit built around a Rust CLI (`hgb`), a resident Rust daemon (`hgbd`), and several alternative execution engines. It includes terminal and browser interfaces, cloud/local model integrations, file and command tools, project memory, verification, recovery, MCP connections, and experimental hardware/distributed features. Its strongest next improvement would be to make workspace handling, execution policy, verification, and recovery consistent across those interfaces.

**Repository snapshot**

| Item | Observed result | Meaning |
| --- | --- | --- |
| Cargo workspace | Five Rust crates, version `0.1.0` | The main application is one multi-crate Rust workspace. |
| Maintained Rust source | 363 files under the five crates' `src/` directories; 163,696 lines | This is a substantial codebase with a broad maintenance surface. Counts exclude generated build trees. |
| Integration-test source | 45 files under crate `tests/` directories | Many feature-specific test suites exist. |
| Test declarations | 1,103 `#[test]` / `#[tokio::test]` attributes across maintained source and integration tests | This is a source count, not a passing-test count or a coverage percentage. |
| Browser frontend | `hgb-nextgen/dist/index.html`, 7,394 lines | A substantial browser application exists alongside the terminal interfaces. |
| Native acceleration | Zig code called through Rust FFI | The core build script attempts a Zig static-library build and falls back when it fails. |
| Browser WebAssembly | Separate `hgb-wasm-kernel` Zig project | It is not a member of the Cargo workspace; its build needs separate verification. |
| MCP configuration | 56 server entries in the local `hagibis.mcp.json` | Configured entries do not prove the services are installed or operational. |

**What each part does**

| Component | Plain-language role | Main evidence | Maintenance observation |
| --- | --- | --- | --- |
| `hgb-cli` | Accept commands, display responses, run the terminal UI, and launch selected local features | [main.rs](../crates/hgb-cli/src/main.rs), [repl.rs](../crates/hgb-cli/src/repl.rs), [client.rs](../crates/hgb-cli/src/client.rs) | `main.rs` has 10,569 lines and `repl.rs` has 5,918; command definitions and execution deserve smaller modules. |
| `hgb-daemon` | Receive requests, choose handlers, keep shared state, and expose network services | [server.rs](../crates/hgb-daemon/src/server.rs), [hgbd.rs](../crates/hgb-daemon/src/bin/hgbd.rs) | One 4,124-line server file coordinates many unrelated subsystems. |
| `hgb-core` | Define requests/providers and implement agent reasoning, file tools, rules, analysis, verification, and integrations | [protocol.rs](../crates/hgb-core/src/protocol.rs), [agent.rs](../crates/hgb-core/src/agent.rs), [crud.rs](../crates/hgb-core/src/crud.rs) | At 247 source files, this is much broader than a small shared core. |
| `hgb-storage` | Store skills, vector information, style preferences, and cached answers | [lib.rs](../crates/hgb-storage/src/lib.rs), [semantic_cache.rs](../crates/hgb-storage/src/semantic_cache.rs) | Some modules re-export core implementations; persistence is not confined to this crate. |
| `hgb-nextgen` | Supply browser/TUI features, healing, checkpoints, sandbox helpers, and additional workflows | [visual_ide.rs](../crates/hgb-nextgen/src/visual_ide.rs), [checkpoint.rs](../crates/hgb-nextgen/src/checkpoint.rs), [heal.rs](../crates/hgb-nextgen/src/heal.rs) | Several responsibilities overlap with `hgb-core`; the browser has its own agent paths. |
| Native Zig and WASM | Accelerate selected computations and provide browser-side operations | [build.rs](../crates/hgb-core/build.rs), [native Zig](../crates/hgb-core/native/zig/hgb_accelerate.zig), [WASM README](../crates/hgb-wasm-kernel/README.md) | Build and runtime capability reporting should make native, fallback, and unavailable modes explicit. |
| CI and documentation | Build/test the project and run its own verification action | [workflow](../.github/workflows/hagibis-ci.yml), [action.yml](../action.yml), [README](../README.md) | Tests exist, but some checks depend on external services and documentation contains stronger guarantees than the inspected paths support. |

The dependency direction is roughly CLI → daemon/nextgen/storage/core, daemon → nextgen/storage/core, nextgen → storage/core, and storage → core. CLI commands can also invoke core features directly. Therefore, the daemon is not the sole execution boundary.

**How a typical request moves through the program**

| Step | What happens | Input → output | Relevant implementation |
| --- | --- | --- | --- |
| 1. Receive intent | Clap parses a CLI command; the REPL accepts interactive input; the browser uses HTTP/WebSocket routes | User text/options → command or API request | `hgb-cli/src/main.rs:220`; `hgb-nextgen/src/visual_ide.rs:465` |
| 2. Choose an execution path | Many CLI commands construct `HgbRequest`; some execute directly in the CLI process | Command → daemon request or local function | `main.rs:4235`, `4783`, `5945` |
| 3. Contact the daemon | The client connects to a Unix socket, tries to start `hgbd` if unavailable, and exchanges length-prefixed Bincode messages | Request → serialized bytes → response | `hgb-cli/src/client.rs:58`, `107` |
| 4. Dispatch | The daemon deserializes and matches the request to a handler | `HgbRequest` → feature handler | `hgb-daemon/src/server.rs:286`, `381` |
| 5. Build context | Prompt handling can add inspected content, matching skills, project memory, and vector matches; agent handling adds rules and a repository map | Prompt + context → model input | `server.rs:490`, `1225` |
| 6. Consult a model or cache | Prompt handling checks the semantic cache and selects a provider; the agent loop requests a completion repeatedly | Context → generated response/tool instructions | `server.rs:575`; `hgb-core/src/agent.rs:269` |
| 7. Perform work | The ReAct engine parses fenced JSON tool calls and runs file/command tools; other workflows have their own executors | Tool instructions → file changes, command output, observations | `agent.rs:404`; `crud.rs:263` |
| 8. Verify/recover where wired | Healing and speculative workflows may invoke checks and checkpoints; ordinary agent completion does not centrally enforce these steps | Candidate change → accepted result or recovery | `hgb-nextgen/src/heal.rs`; `hgb-core/src/unified_driver.rs` |
| 9. Return and retain results | Responses return to the interface; selected paths save sessions, memory, or cached answers | Result → displayed response + stored state | `main.rs:4783`; `hgb-core/src/session.rs:248` |

**Commands that appear similar but behave differently**

| Entry point | Actual inspected behavior | Practical implication |
| --- | --- | --- |
| `hgb run` | Adds optional conversation history, sends a `Prompt` request, then saves the exchange. The daemon routes to providers; the Ollama branch also has a limited tool-call follow-up. | It does not share the full execution contract of `hgb agent`. |
| `hgb agent` | Sends `AgentRun`; the daemon creates a `ReActAgentEngine`, prepares context, and loops through model/tool turns. | This is the main inspected autonomous tool-use path. Its provider selection is narrower than ordinary prompt routing. |
| `hgb do` | Runs `UnifiedAutonomousDriver` locally; generates four template-based candidates, evaluates them, selects one, and optionally writes it. | In the inspected implementation, candidate generation does not call an LLM. Default Rust verification is syntax parsing, not compilation or tests. |
| `hgb heal` | Sends a healing request that runs checks and uses `SwarmCheckpointManager` snapshots during repair attempts. | Recovery is attached to that particular checkpoint mechanism. |
| `hgb undo` versus REPL `/undo` | The CLI uses disk-backed `CheckpointTimeMachine`; REPL `/undo` sends a request using the daemon's in-memory `SwarmCheckpointManager`. | The two commands can see different histories; a daemon restart loses its in-memory checkpoints. |
| Browser IDE | Serves its own routes for editing, models, tools, approvals, Git, terminals, and agent operations. It binds to loopback and has authentication middleware. | Safeguards in this interface should not be assumed to apply to the daemon's separate MCP HTTP service. |
| MCP server/host | Hagibis can expose its own tools and connect to configured external tool servers. | Treat exposed tools and outbound integrations as separate capabilities with explicit policies. |

**Where data is kept**

| Data | Observed location/model | Suggested adjustment |
| --- | --- | --- |
| Chat sessions | Workspace `.hgb/sessions/` JSON files | Keep this workspace association and add schema migration/retention policies. |
| Project memory | Workspace `.hgb/memory.json` | Include a workspace identity in every operation that reads or writes it. |
| CLI time-machine snapshots | Workspace `.hgb/checkpoints/` | Make this part of one shared mutation/recovery API. |
| Daemon healing checkpoints | In-memory vectors/maps in `SwarmCheckpointManager` | Persist the history and restore it after process restarts. |
| Daemon style/skills/vectors/cache | Paths derived from the socket's parent: `style_vault.db`, `skills.db`, `vectors.db`, `cache/semantic_cache.json` | Separate durable state from runtime sockets; explicitly namespace project-specific data. |
| Other feature stores | Multiple `.hgb/` files, including SQLite stores | Inventory ownership and establish one storage/configuration policy. |

Sources: [daemon initialization](../crates/hgb-daemon/src/server.rs) lines 86–185; [session persistence](../crates/hgb-core/src/session.rs) lines 238–311; [checkpoint implementations](../crates/hgb-nextgen/src/checkpoint.rs) and [time machine](../crates/hgb-core/src/checkpoint_time_machine.rs).

**Prioritized findings and suggestions**

These are code-path findings unless explicitly described as test results. “Critical” means fix before relying on the affected path for unattended execution or network exposure; it is not a claim of an observed attack.

| Priority | Finding and evidence | Why it matters | Concrete suggestion | Acceptance check |
| --- | --- | --- | --- | --- |
| Critical | MCP SSE starts automatically and binds `0.0.0.0`; its router/handler has no authentication check and accepts tool calls without requiring a valid session. `mcp_sse_server.rs:110–127,177–278`; startup in `server.rs:299–302`. | Reachable network clients can invoke server-side tools. Actual reachability depends on host/network controls. Startup/status text misleadingly says `127.0.0.1`. | Default to loopback, make remote exposure explicit, require authorization, validate sessions, and return bind/startup failures. | Requests without authorization are rejected; startup reports the real listener address. |
| Critical | `OverlaySandbox::write_file` joins caller-supplied paths without checking containment; absolute or parent-traversal paths can escape the intended directory. `overlayfs_sandbox.rs:306–342`. | Even a speculative write can reach the real filesystem outside the sandbox. | Reject absolute/traversal paths and enforce symlink-aware workspace containment for all reads/writes/deletes. | Absolute paths, `..`, and symlink escapes cannot modify outside files. |
| High | Sandbox directories are derived only from the workspace hash and removed on construction. Hardlink fallback shares file contents, while `execute_command` only changes the process working directory. `overlayfs_sandbox.rs:96–115,260–278,435–440`. | Concurrent sandboxes can delete each other's working state. External programs can modify shared hardlinked files or access outside the working directory. | Use unique per-run directories, independent file copies/reflinks, and an enforced process sandbox. Distinguish workspace staging from OS isolation. | Two parallel sandboxes remain independent; subprocess edits cannot alter the source workspace before commit. |
| High | `hgb do` generates fixed templates and reports syntax verification as `compilation_passed` and a 100% test pass rate. Non-Rust targets can receive unconditional success. `unified_driver.rs:316–377,457–668`. | A successful report does not establish that the requested task was implemented or the program works. | Use provider-generated patches or clearly label template mode. Report syntax/build/test results separately and run the project's real checks. | A syntax-valid but type-invalid Rust candidate is rejected; absent tests are reported as not run. |
| High | Arbitration can select the highest-scoring candidate even if all candidates failed; commit checks only `auto_commit`. Healed candidate text is not returned to the original candidate map used for commit. `graph_of_thought.rs:494–522`; `unified_driver.rs:181–223,263–342`. | Failed or pre-healing code can be selected for writing. | Exclude failed candidates; return the verified artifact from evaluation; revalidate its content hash immediately before applying it. | All-failed candidates produce no commit; committed bytes exactly match the bytes that passed verification. |
| High | `hgb agent` supplies no workspace root, the daemon defaults to `.`, and `Prompt` has no workspace field. The MCP server is initialized with the socket-parent directory. `main.rs:5945–5952`; `protocol.rs:9–15`; `server.rs:299–301,510,1225–1226`. | A daemon started from project A can use the wrong context or directory when called from project B. | Add canonical workspace/session identity to a common request envelope; keep workspace state keyed by that identity. | A daemon started in A correctly handles independent requests from B and C. |
| High | Every client error triggers local re-execution of the same request. `repl.rs:3357–3364`. | If the daemon performs a command but its response is lost, fallback can repeat the side effect. | Fall back only before dispatch is known to have occurred; attach request IDs and deduplicate mutating operations. | Dropping a response after execution does not execute the operation twice. |
| High | `AgentLoopConfig.auto_approve` and `turn_timeout_secs` are declared but never consulted by the engine. Normal command execution only enables Landlock when `HGB_SANDBOXED` is set, and continues without it when support is absent. `agent.rs:49–72,269–344`; `crud.rs:310–337`. | Configuration does not reliably enforce the advertised execution controls. Command substring checks do not confine a process. | Put permission, workspace, deadline, cancellation, and sandbox requirements in one executor used by all interfaces; fail explicitly when a required capability is unavailable. | Disabled auto-approval blocks mutations; stalled provider calls time out; required confinement cannot silently downgrade. |
| High | CLI and REPL undo use different checkpoint stores; normal ReAct writes do not automatically create checkpoints. `main.rs:5965`; `repl.rs:2059`; `server.rs:1312`; `checkpoint.rs:94–112`; `agent.rs:448–480`. | Users cannot depend on one consistent recovery history after any AI edit or restart. | Snapshot through one durable transaction service before mutations and use the same service for every undo entry point. | Edit → restart → undo restores the same files from CLI, REPL, and browser. |
| Medium | Prompt routing discards the explicit `provider`/`stream` fields through `..`; `AgentRun` independently chooses mainly Ollama/Gemini. `server.rs:490,1234–1250`. | A model/provider available in chat may not work in agent mode; requested provider choice may be ignored. | Share a provider registry and honor explicit provider/model/fallback policy across chat and tools. | The same provider/model pair behaves consistently in every supported interface. |
| Medium | The semantic cache uses hashed word vectors and similarity matching, without explicit workspace/revision/expiry fields. Context is included in the query, but no formal invalidation contract exists. `simd_vector_index.rs:57–81`; `semantic_cache.rs:16–34,102–172`. | Similar prompts can retrieve stale or inappropriate answers; project changes are not guaranteed to invalidate them. | Start with exact context-hash caching including workspace, revision, model, rules, and expiry; reserve approximate reuse for clearly safe use cases. | Editing a relevant file invalidates the previous answer; unrelated workspaces do not share contextual results. |
| Medium | Ping returns literal `12` microseconds; status returns literal `8.4` MB. Other paths have real measurements, so this is not a blanket statement about telemetry. `server.rs:383,421`. | These fields cannot substantiate README performance claims. | Measure round-trip latency/RSS and clearly distinguish measured, estimated, and unavailable values. Publish reproducible release benchmarks. | Changing workload changes reported metrics; benchmark output includes revision, build profile, and environment. |
| Medium | Large command/server/UI modules and substantial unconditional dependencies increase coupling. The build passes with many warnings. | Small changes are harder to review and optional features increase build/startup complexity. | Separate commands, providers, execution policy, storage, and optional features; initialize expensive daemon services lazily; reduce warnings progressively. | Core-only builds run basic chat/tools without initializing optional VM, distributed, audio, or hardware services. |
| Medium | The agent execution test returns early without Ollama and has its meaningful outcome assertions commented out. The CI action can reuse an installed `hgb` instead of building the reviewed revision. `agent.rs:854–871`; `action.yml:65–72`. | A green suite can miss broken tool execution or test a stale executable. | Add deterministic provider-contract tests and end-to-end workspace/recovery scenarios; make live integrations explicit; build the current revision in CI. | CI detects a missing tool call and verifies the exact checked-out code. |

An additional documentation correction: BLAKE3 hashes provide content integrity checks, but the inspected `ThoughtCertificate` uses an unkeyed hash and does not prove code correctness or signer identity. Describe the certificate according to what it covers (`graph_of_thought.rs:341–371`). The ordinary CLI transport also allocates serialization/receive buffers (`client.rs:110–128`); the existence of separate shared-memory modules does not make that path zero-copy.

**Suggested development sequence**

| Order | Work package | Useful deliverable |
| --- | --- | --- |
| 1 | Close exposed MCP access; enforce path and process boundaries; isolate concurrent sandboxes | A clearly defined and testable execution boundary. |
| 2 | Fix workspace propagation, duplicate execution, verified-artifact commit, and persistent undo | One dependable flow: request → inspect → propose → verify → apply → recover. |
| 3 | Consolidate provider routing, permission/deadline controls, and cache invalidation | Consistent behavior across terminal, browser, and MCP. |
| 4 | Add deterministic failure-path tests and accurate metrics; align README guarantees | Evidence-based release readiness. |
| 5 | Split oversized modules and make advanced subsystems optional | Easier maintenance and a smaller default runtime. |

**Validation performed**

| Check | Result | Limit |
| --- | --- | --- |
| `cargo check --workspace --locked --offline` | Passed using the local dependency/build cache; compiler warnings remain. | Type-checking is not a full link, clean-room build, or runtime validation. |
| `cargo test -p hgb-storage --lib semantic_cache::tests --locked --offline` | Six passed, zero failed; 29 other storage tests filtered out. The first attempt hit a sandbox read-only linker error; the permitted rerun passed. | Covers exact/similar query hits, dissimilar misses, model isolation, eviction, and persistence; not workspace invalidation or the whole program. |
| Source tracing | Inspected main CLI/daemon/agent/MCP/sandbox/provider-routing/storage/recovery paths and build/CI files. | Findings outside those paths need further feature-specific testing. |
| Live integrations and benchmarks | Not exercised. | No claim that cloud providers, configured MCP servers, Firecracker, distributed features, or README performance numbers were validated. |

Review logs are in `/tmp/hagibis-review-cargo-check.log` and `/tmp/hagibis-review-storage-tests.log` for this session. Application source was not modified; this report is the only repository addition.
