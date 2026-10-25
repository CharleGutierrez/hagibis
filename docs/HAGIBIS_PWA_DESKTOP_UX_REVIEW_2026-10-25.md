# Hagibis PWA and Desktop IDE Review

Reviewed source at `58ad20e` on 25 October 2026. This is a source review plus focused automated tests, not a complete browser usability study. The machine has Firefox and Node, but no Chromium browser or Playwright driver; the live interface could not be opened and inspected visually. The app exposes 87 API routes and the HTML contains 232 inline click handlers, so the focused Rust tests do not establish that every control works end to end.

## What users launch

| Entry point | Actual behavior | User-facing consequence |
| --- | --- | --- |
| `hgb ide [PATH]` (aliases `gui`, `pwa`, `visual-ide`) | Starts `VisualIdeServer` on loopback, defaults to port `4173`, then launches a Chromium `--app` window unless `--no-open` is passed. | This is the normal all-in-one path for the PWA-backed IDE. |
| `hgb desktop-ide` (alias `studio-app`) | Only opens `http://127.0.0.1:38472` in standalone Chromium mode or a browser fallback. It does not start `VisualIdeServer`. | Unless a server was started separately on that port, users see a connection error. Its default does not match `hgb ide`'s port. |
| Installed PWA | Uses the web manifest and service worker served by `hgb ide`. | It is an installed shell around the running local server; API-backed IDE functions are not fully offline. |

## Main user flow

| Stage | User action | Implementation | UX / test observation |
| --- | --- | --- | --- |
| Start | Run `hgb ide [workspace]`. | Starts an Axum server bound to `127.0.0.1`; optionally starts Chromium. | Good default for a local desktop. Startup prints the auth token in the URL/terminal when supplied, which is convenient but can expose it through terminal history/screenshots. |
| Explore | Select files, search, view rules/symbols, inspect Git. | Workspace tree and API-backed panels. | Large single-page IDE surface has many actions; no `aria-label` attributes were found in the HTML. Icon-only controls rely on title text/tooltips. |
| Edit | Use Monaco, split editor, diff, inline edit, or composer. | Save/edit/diff/composer endpoints. | Most workflows depend on live server and providers. Save, diff, and local editor state should show clear dirty/saved/error states. |
| Run | Use terminal, tests, debugger, preview, or agent. | PTY/WebSocket, test runner, DAP, preview and agent APIs. | Powerful, but command and filesystem routes need strict workspace boundaries and access policy. |
| Install/offline | Install from browser prompt; service worker caches shell/assets. | `manifest.json`, `sw.js`, install prompt. | Precache uses external CDNs and catches `addAll` failure; one failed CDN fetch can leave an incomplete cache. App shortcuts are declared but `?action=terminal/agent/composer` is not handled in the HTML. |
| Exit | Close browser and stop server with Ctrl+C. | CLI waits on Ctrl+C; desktop launcher exits after spawning a browser. | The separate launcher does not manage/start/stop the server lifecycle. |

## Focused test results

| Check | Result | Coverage limit |
| --- | --- | --- |
| `cargo test -p hgb-nextgen --lib --locked --offline` | **144 passed, 2 failed** of 146 tests. Failures: `vibe_cockpit_tui::tests::test_headless_buffer_renders_all_four_panes` expected `[s] Toggle Sound`; `voice_hook::tests::test_continuous_vad_listener_stream` expected `Refactor payment handlers` but got `Refactor payment handlers with`. | Broad crate health check, not an exhaustive PWA route/UI suite; the failures are in TUI/voice modules, not `visual_ide`. |
| `cargo test -p hgb-nextgen --lib visual_ide::tests --locked --offline` | Passed: 16 tests, 0 failures, 109.26 seconds. | Exercises selected handlers for diff, file create/delete, composer, sessions, search/replace, PWA assets, WASM headers, preview, Git history, and test discovery/run. Not all 87 API routes or browser interactions. |
| `cargo test -p hgb-cli --lib desktop_ide::tests --locked --offline` | Passed: 2 tests, 0 failures. | Only default config and launcher construction; it does not spawn a browser or verify that a server is reachable. |
| Frontend static checks | Passed: inline JS parsed by Node; service worker passed `node --check`; manifest parsed as JSON. | Syntax/metadata only, not DOM runtime, network behavior, layout, installability, or keyboard interaction. |
| Browser UI / responsive view | Not completed. Headless Firefox launch timed out before producing a screenshot. | No Chromium/Playwright harness; layout, runtime state, keyboard, touch, and visual quality remain unverified in a real rendered session. |

## Findings and suggested fixes

| Priority | Finding | Why it matters | Recommendation / acceptance check |
| --- | --- | --- | --- |
| Critical | `VisualIdeServer` binds to loopback, but CORS replies allow `Access-Control-Allow-Origin: *`. Authentication is optional by default. The API includes file read/write and arbitrary shell command routes; file endpoints accept caller-supplied paths rather than resolving them beneath `workspace_root`. | A page on another site can make browser requests to the local IDE and, when no token is configured, potentially read or change local files and run commands. The loopback bind alone does not prevent browser-origin attacks. | Require authentication by default, restrict CORS to the exact local UI origin, reject cross-origin requests, and jail every path to the selected workspace. Test from a separate origin: file/terminal calls must fail without explicit auth and traversal/absolute paths must be rejected. |
| High | `hgb desktop-ide` is only a browser launcher with default port `38472`; `hgb ide` starts the server on default port `4173`. | A user following the desktop command alone can open an empty/unreachable app window. | Make one command own server and window lifecycle, or name the launcher as an attach-to-running-server command and default it to the same port. Show a useful readiness check and actionable error instead of reporting launch success unconditionally. |
| High | The manifest's three shortcuts use `?action=...`, but client code only reads `token` from the query string. | Shortcut taps open the IDE but do not navigate to their advertised Terminal/Agent/Composer destination. | Parse the `action` parameter during startup, route to the requested view, and add a smoke test for each shortcut URL. |
| High | PWA service worker precaches external CDNs in one `cache.addAll`; failure is swallowed, allowing install/activation to continue with only part of the cache. | Offline startup can fail unpredictably depending on CDN availability. PWA backend calls also still need the local daemon/server. | Bundle critical JS/CSS locally, separate optional CDN cache, show offline/server status, and test first install with CDN offline plus later reconnect. Describe offline limits accurately. |
| Medium | Layout queries are designed for laptop widths (1440/1250/950), while manifest advertises any orientation and mobile categories. Three major columns remain on narrow widths. | Small tablet/phone screens are likely cramped; no real viewport review was possible. | Add a mobile breakpoint that turns sidebars into toggleable drawers and preserves editor width; validate 375, 768, 1024 and 1440 px screenshots. |
| Medium | The UI has 232 inline click handlers and zero `aria-label` attributes; visible controls include many icon-only actions. | Keyboard and screen-reader use, focus visibility, and touch target discoverability are hard to verify and likely uneven. | Add semantic buttons/labels, visible focus, keyboard access, reduced-motion support, and automated accessibility checks. Prefer delegated listeners/components over inline handler strings. |
| Medium | Desktop launcher creates an isolated profile but ignores profile-directory creation failure; fallback launcher spawn failure is also ignored and still returns a success-shaped report. | The UI can claim launch succeeded even if profile setup or app launch failed. | Propagate profile and spawn errors; probe server readiness; report whether the app window actually launched or only a URL was opened. |
| Medium | Existing tests cover a small subset of API routes and backend handler calls, not end-to-end controls. | A handler can pass while the UI calls the wrong route or fails to render an error. | Add Playwright smoke flows for launch/auth, file open/edit/save/reopen, search/replace/rollback, terminal, test run, Git status/stage, agent approval, PWA shortcut, offline state, and desktop launcher. |

## Recommended test sequence

| Order | Scenario | Pass condition |
| --- | --- | --- |
| 1 | Start `hgb ide` with a temporary workspace | Server responds on the printed port; wrong port and occupied port produce clear errors. |
| 2 | Load UI and check workspace tree | Tree shows only workspace files; hidden/system exclusions behave as intended. |
| 3 | Edit, save, reload, diff, and rollback a fixture | Exact bytes persist; diff is correct; rollback restores the checkpoint. |
| 4 | Exercise terminal, tests, Git, agent approvals, debugger, and preview | Each control reaches its expected route and surfaces failures without false success. |
| 5 | Probe hostile origin and path inputs | No unauthenticated cross-origin operation; no access outside workspace. |
| 6 | Install PWA, use shortcuts, disconnect network, reconnect | Each shortcut opens its named view; shell behavior and backend-required features are clearly described. |
| 7 | Launch desktop mode and close/restart it | Server/window lifecycle is predictable; failures are visible; no orphan server/profile issue. |
| 8 | Inspect mobile, keyboard, screen reader and contrast | Layout remains usable at phone/tablet widths; all actions have accessible names and focus states. |

Source review references: `crates/hgb-cli/src/main.rs`, `crates/hgb-cli/src/desktop_ide.rs`, `crates/hgb-nextgen/src/visual_ide.rs`, and `crates/hgb-nextgen/dist/{index.html,manifest.json,sw.js}`.
