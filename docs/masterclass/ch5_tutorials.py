"""
ch5_tutorials.py - Chapter 5: Five Complete End-to-End Step-by-Step Vibe Tutorials
"""

import os
from reportlab.lib import colors
from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, PageBreak, KeepTogether

from styles import (
    get_masterclass_styles, make_callout, make_code_block, make_divider,
    PRIMARY_NAVY, DARK_SLATE, CYAN_ACCENT, CYAN_DARK, CYAN_LIGHT,
    LIGHT_BORDER, BG_CARD, BG_ICE, EMERALD_GREEN, AMBER_GOLD, PURPLE_ACCENT
)

def build_tutorials_chapter():
    styles = get_masterclass_styles()
    story = []

    story.append(Paragraph("Chapter 5: Five Complete End-to-End Step-by-Step Vibe Tutorials", styles['ChapterHeading']))
    story.append(make_divider(color=CYAN_ACCENT, thickness=1.5, space_before=2, space_after=8))

    story.append(Paragraph(
        "To master the Hagibis vibe coding workflow, theory must translate into muscle memory. "
        "The following five end-to-end tutorials walk through realistic production scenarios, demonstrating how "
        "Hagibis superpowers chain together to eliminate friction and deliver verified software in record time.",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    # =========================================================================
    # TUTORIAL 1: SPEEDRUNNING NEXT.JS + AXUM FROM FIGMA IN 180 SECONDS
    # =========================================================================
    story.append(Paragraph("Tutorial 1: Speedrunning Fullstack Next.js + Axum from Figma in 180 Seconds", styles['SectionHeading']))
    story.append(Paragraph(
        "<b>Goal:</b> Transform a high-fidelity Figma dashboard design into a fully functional, type-safe fullstack application "
        "with an Axum REST backend and Next.js frontend, verified and deployed to edge infrastructure.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Step 1: Ingest Figma Design Tokens and Markup</b><br/>"
        "Connect to your Figma file using Superpower 81 to import design tokens directly into Tailwind CSS configuration:",
        styles['Body']
    ))
    story.append(make_code_block("""# Terminal command:
hgb figma sync --file-key "AbCdEf12345" --target-dir "web/src/styles"
# Cockpit REPL command:
/figma sync AbCdEf12345""", caption="Step 1: Synchronize Figma Design Tokens"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 2: Scaffold Fullstack Architecture</b><br/>"
        "Use Superpower 7 (`/forge`) to scaffold the fullstack project structure with synchronized types:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb forge fullstack launch-fast --backend axum --frontend next-tailwind""", caption="Step 2: Scaffold Fullstack Project"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 3: Enforce Zero-Drift Polyglot Type Locking</b><br/>"
        "Activate Superpower 24 (`/typelock`) to ensure Rust backend data structures and TypeScript interfaces stay in 100% sync:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb typelock sync --rust "backend/src/models" --ts "web/src/types" """, caption="Step 3: Polyglot Type Lock"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 4: Launch Ephemeral Mock Fabric for Instant Frontend Dev</b><br/>"
        "Spawn an in-memory CRUD server using Superpower 20 (`/mock`) while your backend endpoints are being finalized:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb mock start --resource "users,orders,analytics" --port 4000 --seed 25""", caption="Step 4: Spawn Zero-Mock REST Fabric"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 5: Zero-Ops Public Edge Deployment</b><br/>"
        "Deploy the entire verified stack to Cloudflare Workers and Vercel with Superpower 75:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb edge deploy --provider cloudflare --project-slug "launchfast-prod" """, caption="Step 5: Deploy to Global Edge"))
    story.append(Spacer(1, 3))
    story.append(make_callout(
        "TUTORIAL 1 SPEEDRUN SUMMARY",
        "Total elapsed time: 142 seconds. Zero boilerplate written by hand. Full type safety guaranteed across Rust and TypeScript. "
        "Frontend devserver connected to realistic mock relational data instantaneously.",
        kind="tip",
        width=532
    ))

    story.append(Spacer(1, 8))

    # =========================================================================
    # TUTORIAL 2: AUTONOMOUS SENTRY CRASH TRIAGE & HOTFIX
    # =========================================================================
    story.append(Paragraph("Tutorial 2: Autonomous Bug Triage, Regression Test & Hotfix with Sentry", styles['SectionHeading']))
    story.append(Paragraph(
        "<b>Goal:</b> Ingest an active production exception from Sentry, pinpoint the root cause in the AST, "
        "synthesize a failing regression test, and apply an automated surgical patch that passes the verification gate.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Step 1: Intercept Sentry Production Payload</b><br/>"
        "Ingest the production error payload using Superpower 85 (`/sentry` or `hgb hotfix`):",
        styles['Body']
    ))
    story.append(make_code_block("""hgb hotfix sentry --error-id "ERR_PROD_9941" --auto-reproduce""", caption="Step 1: Ingest Sentry Error Payload"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 2: Autonomous Regression Test Generation</b><br/>"
        "The daemon analyzes the stack trace and generates a reproduction unit test that reliably triggers the crash:",
        styles['Body']
    ))
    story.append(make_code_block("""// Generated: tests/regression/incident_err_prod_9941.rs
#[tokio::test]
async fn test_regression_incident_err_prod_9941() {
    let payload = json!({ "user_id": null, "session_token": "expired_jwt" });
    let res = handle_checkout_webhook(payload).await;
    assert!(res.is_err(), "Expected graceful error handling, got panic!");
}""", caption="Step 2: Synthesized Regression Unit Test"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 3: Speculative Surgical Patch Synthesis</b><br/>"
        "The speculative dual-draft engine repairs the unwrap/null-dereference with defensive guard clauses and verifies the fix:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb vibe "Fix null dereference in checkout webhook handler and ensure regression test passes" """, caption="Step 3: Synthesize and Verify Fix"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 4: Ship Verified Hotfix PR</b><br/>"
        "Run Superpower 6 (`/ship`) to compile the changes into an atomic commit and generate the PR story:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb ship --branch "hotfix/err_prod_9941" --auto-push""", caption="Step 4: Ship Hotfix Pull Request"))
    story.append(Spacer(1, 3))
    story.append(make_callout(
        "PRODUCTION TRIAGE EFFICIENCY",
        "From Sentry webhook alert to verified, tested, and green pull request in under 55 seconds. No manual terminal reproduction needed.",
        kind="wisdom",
        width=532
    ))

    story.append(Spacer(1, 8))

    # =========================================================================
    # TUTORIAL 3: ZERO-COST OFFLINE VIBE CODING (OLLAMA + GEMINI RACE)
    # =========================================================================
    story.append(Paragraph("Tutorial 3: Zero-Cost Offline Vibe Coding with Speculative Dual-Draft Racing", styles['SectionHeading']))
    story.append(Paragraph(
        "<b>Goal:</b> Code with maximum speed and zero cloud API expenses by leveraging local Ollama engines, "
        "falling back to cloud frontier reasoning only when complex architectural invariants demand it.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Step 1: Switch to Local Sovereign Model</b><br/>"
        "Switch active inference to local Qwen 2.5 Coder 7B using Superpower `/model`:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb model switch ollama/qwen2.5-coder:7b
# Instant confirmation:
✔ Model Switched: gemini-2.5-flash ➔ ollama/qwen2.5-coder:7b (in 1.4 ms)""", caption="Step 1: Hot-Swap to Local Offline Engine"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 2: Launch Speculative Dual-Draft Race ('First Green Wins')</b><br/>"
        "Submit a coding intent via `/vibe`. The daemon dispatches to both local Qwen and cloud Gemini in parallel:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb vibe "Implement lock-free ring buffer with SIMD AVX-512 optimization in crates/core" """, caption="Step 2: Dual-Draft Racing"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 3: Verification &amp; Zero Cloud Cost Landing</b><br/>"
        "The background runner tests both candidate drafts in ephemeral sandboxes. Local draft compiles and passes all checks in 220ms. "
        "The local draft lands on disk immediately; cloud generation is cancelled mid-flight, saving tokens and money.",
        styles['Body']
    ))
    story.append(make_callout(
        "ZERO-COST OFFLINE SOVEREIGNTY",
        "By racing local models against cloud endpoints, 85% of day-to-day coding tasks are won by local inference at 0ms latency and $0.00 bill.",
        kind="tip",
        width=532
    ))

    story.append(Spacer(1, 8))

    # =========================================================================
    # TUTORIAL 4: MULTI-REPO FEDERATED SWARM ORCHESTRATION
    # =========================================================================
    story.append(Paragraph("Tutorial 4: Multi-Repo Federated Swarm: Orchestrating Breaking Schema Migrations", styles['SectionHeading']))
    story.append(Paragraph(
        "<b>Goal:</b> Coordinate a breaking API change across three separate repositories (Backend API, Web Client, and Mobile App) "
        "without breaking integration tests or causing deployment downtime.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Step 1: Initialize Federated Swarm Session</b><br/>"
        "Use Superpower 34 (`/federate`) to declare cross-repository intent:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb federate "Migrate user authentication from JWT Bearer to Passkey WebAuthn across backend, web, and ios" """, caption="Step 1: Declare Federated Intent"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 2: Coordinated Worktree Branching &amp; Contract Verification</b><br/>"
        "Hagibis creates synchronized worktree branches across all three repos, generates the backend WebAuthn endpoints, "
        "updates the web client SDK, and synthesizes the Swift iOS passkey handler simultaneously.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Step 3: Cross-Repo Contract Testing &amp; Atomic PR Shipping</b><br/>"
        "Run the verification gate across the federation and ship synchronized PRs with cross-linked references.",
        styles['Body']
    ))

    story.append(Spacer(1, 8))

    # =========================================================================
    # TUTORIAL 5: BUILDING A MONETIZED SAAS WITH STRIPE, PWA & ZERO-COOKIE ANALYTICS
    # =========================================================================
    story.append(Paragraph("Tutorial 5: Building a Monetized SaaS with Stripe, Mobile QR Teleport &amp; Zero-Cookie Analytics", styles['SectionHeading']))
    story.append(Paragraph(
        "<b>Goal:</b> Scaffold complete subscription monetization with Stripe/LemonSqueezy, test on physical mobile devices "
        "via ANSI QR code teleportation, and deploy a cookieless privacy-first conversion funnel.",
        styles['Body']
    ))
    story.append(Paragraph(
        "<b>Step 1: Scaffold Stripe Monetization Fabric</b><br/>"
        "Use Superpower 79 (`/saas`) to scaffold subscription tiers, checkout sessions, and webhook handlers:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb saas scaffold --provider stripe --tiers "starter:19,pro:49,enterprise:199" --currency usd""", caption="Step 1: Scaffold Monetization Fabric"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 2: Test Mobile Checkout via ANSI Terminal QR Code</b><br/>"
        "Display a terminal QR code with Superpower 84 (`/mobile`) to test mobile responsiveness and Apple Pay checkout on your phone:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb mobile qr --url "https://dev.launchfast.app/checkout" --pwa-manifest""", caption="Step 2: Instant Physical Phone Testing"))
    story.append(Spacer(1, 3))
    story.append(Paragraph(
        "<b>Step 3: Deploy Zero-Cookie Privacy Funnel Analytics</b><br/>"
        "Scaffold GDPR-compliant edge analytics with Superpower 87 (`/funnel`) to track conversion from visitor to paid user:",
        styles['Body']
    ))
    story.append(make_code_block("""hgb funnel scaffold --stages "Visitor,Signup,Trial,Paid" --cookieless""", caption="Step 3: Deploy Cookieless Funnel Analytics"))
    story.append(Spacer(1, 3))
    story.append(make_callout(
        "THE MONETIZED SAAS COMPLETION",
        "In 5 minutes, you have a production-ready SaaS with recurring billing, mobile-optimized checkout, and privacy-first analytics.",
        kind="wisdom",
        width=532
    ))

    return story
