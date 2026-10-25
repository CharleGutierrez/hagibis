**Hagibis — current architecture, process, and recommendations**

Revalidated on 25 October 2026 against committed revision `60eb08a`. The working tree was clean when this pass began. The prior pass examined `4272525` plus the portability/onboarding changes that are now committed. This is the current review; the 24 October document describes an earlier implementation. No application code was changed for this review.

Hagibis is an AI-assisted development environment implemented mainly in Rust. It offers a command-line application (`hgb`), a resident daemon (`hgbd`), terminal and browser interfaces, model/provider integrations, file and command tools, project memory, MCP connectivity, verification, and recovery. The main architectural issue is inconsistent integration: useful controls now exist, but different execution paths do not all use them.

The highest-value next step is to complete one reliable workflow across all interfaces: **identify the project → collect context → propose work → authorize → snapshot → execute in isolation → verify the exact result → apply → retain recovery history**.

**Folder map**

| Folder/file | Role | Observation |
| --- | --- | --- |
| `crates/hgb-cli/` | Commands, REPL, terminal rendering, IDE configuration | Entry point for most user actions. Some features run here directly instead of through the daemon. |
| `crates/hgb-daemon/` | Unix-socket service, shared state, MCP HTTP service, background capabilities | Handles requests, but also constructs many optional engines at startup. |
| `crates/hgb-core/` | Agent reasoning, providers, protocol, file tools, analysis, sandboxing, checkpoints | The largest package; contains both infrastructure and many individual features. |
| `crates/hgb-storage/` | Skills, vectors, style preferences, semantic cache | Persistence also exists in core and nextgen modules. |
| `crates/hgb-nextgen/` | Browser IDE, TUI, healing, additional sandbox/recovery/workflow engines | Several responsibilities overlap with core implementations. |
| `crates/hgb-core/native/zig/` | Native acceleration through Rust FFI | `build.rs` attempts a Zig static-library build and permits a fallback. |
| `crates/hgb-wasm-kernel/` | Separate Zig browser WebAssembly project | Outside the five-member Cargo workspace. Requires separate build/test coverage. |
| `.hgb/` | Local project state | Includes sessions, memory, checkpoints, and feature-specific stores. |
| `hagibis.mcp.json` | External-tool configuration | A configuration entry is not evidence that a service works. Credentials were not reproduced in this report. |
| `.github/workflows/`, `action.yml` | CI and the reusable Hagibis action | Builds/tests need to exercise the reviewed revision and meaningful failure cases. |
| `README.md`, `BLUEPRINT.md`, `docs/` | Architecture and user documentation | Behavioral guarantees should be updated from actual integration tests. |

| Size measure | Current count | Interpretation |
| --- | --- | --- |
| Cargo workspace members | 5 | Core, storage, nextgen, daemon, CLI. |
| Rust source files under their `src/` trees | 366 | Generated build trees excluded. |
| Rust application-source lines | 166,833 | Includes inline tests, comments, and blank lines. |
| Integration-test source files | 45 | Separate from inline tests. |
| Test attributes in maintained Rust source/tests | 1,133 | Declaration count, not executed tests or coverage. |
| CLI entry-point size | 10,686 lines | `hgb-cli/src/main.rs`. |
| Daemon server size | 4,658 lines | `hgb-daemon/src/server.rs`. |
| Browser frontend | 7,394 lines | `hgb-nextgen/dist/index.html`. |

Dependency direction: CLI depends on daemon/nextgen/storage/core; daemon depends on nextgen/storage/core; nextgen depends on storage/core; storage depends on core. The CLI also invokes some core engines directly. A safety feature implemented only in the daemon therefore does not automatically cover every workflow.

**The current process, step by step**

| Step | What happens | Output | Important qualification |
| --- | --- | --- | --- |
| 1. Receive the request | Clap parses CLI commands; REPL and browser routes accept interactive input. | Command or API request. | The interfaces have different dispatch paths. |
| 2. Attach project context | `HgbClient::send` wraps ordinary requests with a UUID and `WorkspaceIdentity`. | An `Enveloped` request. | Some direct/local workflows do not pass through this client. |
| 3. Reach the daemon | Client connects to a Unix socket, optionally auto-starts `hgbd`, and sends length-prefixed Bincode. | Deserialized request. | This ordinary transport allocates buffers; separate SHM features do not make it universally zero-copy. |
| 4. Dispatch | Daemon unwraps context and selects a handler. Selected mutating requests use a completed-response cache. | Feature-specific operation. | Context and retry protection are not applied consistently to every handler. |
| 5. Gather context | Depending on the route: project rules, repository map, memory, skills, inspected files, and vector matches. | Augmented model input. | Prompt handling still reads the daemon's current directory in several places. |
| 6. Select a model or cached answer | Prompt and agent handlers use provider-selection logic. | Text or tool instructions. | A new shared router exists, but these handlers still use separate routing logic. |
| 7. Execute tools | ReAct parses fenced JSON tool calls and dispatches file operations, commands, and extensions. | Observations and possible file changes. | Approval/deadline controls exist in the engine; daemon `AgentRun` still sets automatic approval. |
| 8. Capture recovery state | Agent mutation handling and CRUD writes attempt durable snapshots. | `.hgb/checkpoints/<uuid>/`. | Snapshot errors are ignored, and the two layers can snapshot different roots or duplicate work. |
| 9. Stage and check changes | Speculative engines prepare temporary workspaces and run selected checks. | Check status and a proposed result. | Workspace copies, path checks, and OS process confinement are different mechanisms. |
| 10. Apply and recover | Selected engines copy/rename files back; CLI undo restores durable checkpoints. | Modified project or restored files. | REPL undo still uses another store; some engines ignore write/commit errors. |
| 11. Display and remember | Interfaces show results; selected paths save sessions, memory, and cached responses. | Resumable history and feedback. | Success labels must distinguish syntax, compilation, tests, and successful application. |

Evidence: [client](../crates/hgb-cli/src/client.rs), [protocol](../crates/hgb-core/src/protocol.rs), [daemon dispatch](../crates/hgb-daemon/src/server.rs), [agent](../crates/hgb-core/src/agent.rs), [CRUD](../crates/hgb-core/src/crud.rs).

**What the main commands actually mean**

| Entry point | Inspected behavior | Best way to understand it |
| --- | --- | --- |
| `hgb run` | Sends a prompt, optionally loads conversation history, and saves the exchange. | Chat/prompt workflow with provider-specific behavior. |
| `hgb agent` | Runs a model/tool reasoning loop with rules and repository context. | Autonomous tool-use workflow. |
| `hgb do` | Synthesizes four template-based candidate implementations, evaluates them, chooses one, and optionally applies it. | A strategy/template engine. Candidate synthesis here still does not call an LLM. |
| `hgb vibe` | Can launch a UI/watch workflow or invoke `VibePromptRunner` to synthesize and check a change. | A separate code-generation engine with its own verification/apply behavior. |
| `hgb heal` | Runs a repair workflow and uses the daemon's `SwarmCheckpointManager`. | Another repair/recovery path. |
| `hgb undo` | Uses `DurableCheckpointManager`, with a fallback to the legacy time machine on error. | Persistent recovery, but fallback needs narrower error handling. |
| REPL `/undo` | Dispatches to the daemon's in-memory checkpoint manager. | Currently different history from CLI undo. |
| Browser IDE | Has its own edit, terminal, model, agent, approval, Git, and recovery routes. | Another frontend with partly separate execution engines. |
| MCP server/host | Exposes Hagibis tools and connects to external tool services. | Integration surface, requiring the same project and execution policy as the other interfaces. |

**Progress since the earlier review**

“Implemented” below means visible in the execution path, not full production certification.

| Area | Current status | Evidence and remaining limitation |
| --- | --- | --- |
| MCP network access | Implemented improvement | MCP HTTP binds to `127.0.0.1`; `/sse` and `/message` require a bearer token. The earlier unauthenticated all-interface exposure finding no longer describes this code. See `mcp_sse_server.rs:213–267`. |
| Unix-socket ownership/lifecycle | Implemented improvement | Socket/PID permissions are set to `0600`, existing listeners are checked, and termination cleanup exists. Cleanup scope still needs correction. See `server.rs:386–554`. |
| Workspace propagation | Partial | Envelopes carry workspace identity; `AgentRun`, `Heal`, and `RunCommand` use it. Prompt context and many other handlers still use process CWD or `.`. |
| Path boundary checks | Partial | `PathJail` is used in `SpeculativeSandbox`. `OverlaySandbox` imports it but its read/write/delete methods still directly join paths. |
| Sandbox name collisions | Implemented improvement | Overlay directories now include a UUID. Broad daemon cleanup can still remove another active run's directory. |
| Agent controls | Implemented at engine level | Provider-call timeout and mutation-approval logic are present. Caller policy and dynamic-tool classification remain incomplete. |
| Recovery | Partial | Durable checkpoints are attempted by agent/CRUD and used by CLI undo. Daemon healing/REPL undo remain separate. |
| `hgb do` verification | Improved, still incomplete | Detects real project test commands, reports `Unverified` when none is configured, and refuses failed/unverified commits by default. Healed bytes are still not returned to the candidate map used for commit. |
| Cache/router consolidation | Partial | Context fields/invalidation and a shared provider router exist. Main prompt cache/routing paths do not fully use them. |
| Telemetry | Partial | Status now measures Linux RSS. `Ping` still returns literal `12` microseconds. |

**Recommendations, ordered by impact**

| Priority | Current finding | Why it matters | Suggested change | Completion check |
| --- | --- | --- | --- | --- |
| Critical | Overlay read/write/delete paths still use unchecked joins despite importing `PathJail`. See `overlayfs_sandbox.rs:309–389`. | Absolute paths, parent traversal, and some symlink cases can reach outside the intended workspace. | Enforce containment in every overlay operation and audit commit paths; use filesystem operations that remain safe against symlink races. | Actual overlay APIs reject escape paths without changing an outside marker file. |
| High | Overlay hardlink fallback remains; its subprocess runner only sets CWD. `SpeculativeSandbox` also executes commands by changing CWD alone. See `overlayfs_sandbox.rs:267–281,438–444`; `speculative_sandbox.rs:179–195`. | A temporary working directory alone does not confine subprocesses; shared hardlinks can propagate writes before commit. | Use independent copies/reflinks and a common process sandbox with explicit filesystem/network policy. | A subprocess cannot write outside allowed roots or alter original hardlinked contents. |
| High | Unified-driver healing mutates `current_code` but returns only scores/status/output; the commit later reads the original `branch_code_map`. Its hash therefore covers the selected original bytes, not necessarily the tested healed bytes. See `unified_driver.rs:240–278,381–519`. | A passed repair can lead to application of the pre-repair candidate. | Return a verified artifact containing final bytes, source revision, executed checks, and content hash; stage exactly that artifact. | Force a repair, then assert that applied bytes equal the bytes verified after repair. |
| High | Workspace context is incomplete. Prompt grounding/memory use CWD; MCP initializes with the socket-parent directory; CRUD snapshots use CWD. See `server.rs:446–448,728–755`; `crud.rs:537–540,675–678`. | A shared daemon can use project A's context or recovery store while editing project B. | Require a canonical workspace context throughout handlers, providers, file tools, checkpoints, cache, and MCP sessions. | Start daemon in A and operate on B; all context, edits, and snapshots stay in B. |
| High | Retry protection is incomplete. Client creates the UUID inside `send`, REPL fallback re-executes the original request in a new local state, and daemon deduplication checks before execution but only records after completion. The mutation list also omits operations such as undo and vibe prompting. See `client.rs:102–107`; `repl.rs:3357–3364`; `server.rs:569–588`; `protocol.rs:1570–1584`. | Lost replies and simultaneous duplicate requests can repeat side effects. | Create one ID before dispatch, retain it across attempts, atomically register in-flight operations, persist relevant outcomes, and avoid replay after uncertain dispatch. | Two concurrent requests with one ID execute once; losing the response does not repeat the command. |
| High | Recovery remains fragmented and failures can be hidden. Agent/CRUD ignore snapshot errors; REPL undo uses an in-memory store; CLI falls back on any durable-restore error. See `agent.rs:449–453`; `crud.rs:537–540`; `server.rs:1529–1545`; `main.rs:5982–6000`. | Users may edit without a valid recovery point or receive fallback behavior after a genuine integrity/storage failure. | Use one workspace-aware checkpoint transaction; propagate errors and permit legacy fallback only for explicitly identified legacy formats. | Edit → restart → undo behaves consistently through CLI, REPL, and browser; corrupt checkpoints stop safely. |
| High | Daemon cleanup removes every matching `/dev/shm/hgb_*` or `/dev/shm/vm_*` directory it can delete. See `server.rs:354–371`. | Stopping one daemon can delete another active job's workspace. | Track resources owned by each daemon/run, verify ownership and liveness, and remove only those resources. | Stopping run A preserves run B's directory and contents. |
| High | `VibePromptRunner` ignores some sandbox write/commit errors and labels successful checks as verified; `--no-tests` can supply `true`, and defaults include syntax/lint/build-only commands. See `vibe_prompt_runner.rs:383–461,786–858`; `main.rs:5503–5508`. | Reported success may not establish tested behavior or successful application. | Separate `syntax`, `build`, `tests`, and `applied` outcomes; propagate I/O failures; mark deliberately skipped checks as skipped. | A denied write is an error; `--no-tests` never reports test verification; syntax-only runs are labeled accurately. |
| Medium | Cache recording uses `record_entry`, which stores `context_key: None`; lookup filters by model/similarity only. Workspace invalidation cannot evict those unscoped entries. See `server.rs:786,1143`; `semantic_cache.rs:106–145,165–215`. | Answers can be stale or inappropriate after project changes. | Key by workspace, relevant content/revision, model, rules, and expiry; enforce the key during lookup, recording, and invalidation. | Same prompt in two projects stays separate; a relevant file edit invalidates its old answer. |
| Medium | Shared provider router is exported but the principal prompt/agent handlers still use separate routing. Explicit prompt `provider`/`stream` fields are discarded by `..`. See `providers/router.rs`; `server.rs:700,1442–1463`. | Model choice can work in one interface and fail or change in another. | Make provider identity/capabilities explicit and use one tested router with deliberate fallback policy. | The same requested provider/model resolves consistently through chat, agent, and browser. |
| Medium | Approval coverage relies on a hard-coded tool-name list; registered dynamic tools are not classified by capability. Daemon `AgentRun` sets `auto_approve: true`. See `agent.rs:139–155,388–453`; `server.rs:1470–1476`. | Engine controls may not represent the user's intended policy across extension tools. | Give tools explicit read/write/execute capabilities; pass user policy into every executor, including dynamic tools. | A registered write tool requires the same authorization and checkpoint as a built-in write. |
| Medium | CI can reuse an already-installed `hgb`; the live Ollama test still has key assertions commented out. Large entry points and unused code warnings obscure integration omissions. See `action.yml:65–72`; `agent.rs:1031–1047`. | Helper tests can pass while real command paths remain broken or stale. | Build the checked-out revision; add deterministic full-route tests; split command/server modules by domain; make optional engines lazy. | CI catches disconnected guards, wrong-project operations, dropped replies, and failed recovery. |

The critical/high findings above were established by tracing source paths. Except where test results are explicitly recorded below, they were not exercised against a running daemon or a real user workspace.

**Data and recovery ownership**

| Data | Location/owner | Recommended rule |
| --- | --- | --- |
| Session history | Workspace `.hgb/sessions/` | Namespace and migrate by workspace/session. |
| Project memory | Workspace `.hgb/memory.json` | Use the caller's workspace; define how memory relates to undo. |
| Durable snapshots | Workspace `.hgb/checkpoints/` | Share one durable format and transaction API across interfaces. |
| Legacy daemon snapshots | In-memory `SwarmCheckpointManager` | Migrate or explicitly label as temporary legacy history. |
| Style, skills, vectors, semantic cache | Several daemon paths derive from the socket's parent | Separate runtime files from durable state; distinguish global preferences from project context. |
| Auth tokens/configuration | User configuration and environment | Keep private permissions and explicit rotation/error handling; never substitute predictable randomness when secure token generation fails. |

**Suggested implementation order**

| Stage | Deliverable | How to know it is finished |
| --- | --- | --- |
| 1. Finish boundaries | Shared workspace/path/process policy, safe cleanup, and stable request identity | Outside writes, cross-project actions, concurrent duplicates, and cleanup interference are rejected in end-to-end tests. |
| 2. Finish the transaction | Durable pre-change checkpoint, verified artifact, controlled apply, consistent undo | The exact tested bytes are applied; restart and recovery work through every interface. |
| 3. Unify the AI layer | Shared provider router, contextual cache, capability-aware tools | Model choice, cache reuse, and permissions are predictable across interfaces. |
| 4. Simplify and measure | Smaller modules, optional services, accurate status, reproducible CI/benchmarks | The default path is easy to inspect and documented guarantees have measured evidence. |

For everyday usability, organize the command catalog around a small task menu: **ask**, **inspect**, **propose**, **check**, **apply**, **undo**, **status**. Keep advanced hardware/distributed features discoverable separately. This gives users a clear process while preserving the existing feature set.

**Validation for this review**

| Check | Result | Limit |
| --- | --- | --- |
| `cargo check --workspace --locked --offline` | Passed; the log reports 100 core warnings, including the unused overlay `PathJail` import. | Type-checking against local cached dependencies, not full runtime certification. |
| `cargo test -p hgb-core --lib path_jail::tests --locked --offline` | Passed: 4 tests, 0 failures. The initial sandbox linker error was resolved by the permitted retry, which completed after the build lock cleared. | Validates the path-checking helper; does not show that `OverlaySandbox` calls it. |
| `cargo test -p hgb-core --lib --locked --offline -- agent::tests:: durable_checkpoint::tests:: --skip test_react_agent_execution_loop` | Passed: 4 tests, 0 failures. Covers tool-call extraction, approval-event handling, provider timeout, and durable checkpoint restoration. | The live Ollama test was excluded. The approval test checks the approval event; these tests are not an end-to-end audit of every interface or failure case. |
| Source tracing | Rechecked CLI/daemon dispatch, MCP authorization, workspace handling, overlay boundaries, verified-artifact selection, cache integration, recovery, and CI against `60eb08a`. The wider folder/process inventory remains applicable. | Not an exhaustive audit of every module. |
| Live integrations and benchmarks | Not run. | No new claims about provider availability, external MCP services, VM/distributed features, or advertised speed/memory figures. |

Earlier-pass logs use `/tmp/hagibis-review-2026-10-25-*`; revalidation logs use `/tmp/hagibis-review-60eb08a-*`. This report is the only repository file changed in the revalidation pass.
