"""
ch7_appendix.py - Chapter 7: Appendix, CLI Reference Manual & The Vibe Coder's Oath
"""

import os
from reportlab.lib import colors
from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, PageBreak, KeepTogether

from styles import (
    get_masterclass_styles, make_callout, make_code_block, make_divider,
    PRIMARY_NAVY, DARK_SLATE, CYAN_ACCENT, CYAN_DARK, CYAN_LIGHT,
    LIGHT_BORDER, BG_CARD, BG_ICE, EMERALD_GREEN, AMBER_GOLD, PURPLE_ACCENT
)

def build_appendix_chapter():
    styles = get_masterclass_styles()
    story = []

    story.append(Paragraph("Chapter 7: Appendix, CLI Manual &amp; The Vibe Coder's Oath", styles['ChapterHeading']))
    story.append(make_divider(color=CYAN_ACCENT, thickness=1.5, space_before=2, space_after=8))

    story.append(Paragraph("7.1 Environment Variables Reference", styles['SectionHeading']))
    story.append(Paragraph(
        "Hagibis can be customized via standard Unix environment variables. All variables are optional with production-grade defaults:",
        styles['Body']
    ))
    story.append(Spacer(1, 4))

    env_data = [
        [
            Paragraph("<b>Variable Name</b>", styles['TableHeader']),
            Paragraph("<b>Default Value</b>", styles['TableHeader']),
            Paragraph("<b>Purpose &amp; Description</b>", styles['TableHeader'])
        ],
        [
            Paragraph("<code>HGB_SOCKET_PATH</code>", styles['TableCellCode']),
            Paragraph("<code>/tmp/hgbd.sock</code>", styles['TableCell']),
            Paragraph("Path to Unix Domain Socket for IPC communication between CLI and resident daemon.", styles['TableCell'])
        ],
        [
            Paragraph("<code>HGB_MODEL</code>", styles['TableCellCode']),
            Paragraph("<code>auto</code>", styles['TableCell']),
            Paragraph("Default AI model. 'auto' enables intelligent dual-brain failover between Ollama and Gemini.", styles['TableCell'])
        ],
        [
            Paragraph("<code>GEMINI_API_KEY</code>", styles['TableCellCode']),
            Paragraph("<i>None (Uses OAuth)</i>", styles['TableCell']),
            Paragraph("Optional explicit API key for Google Gemini Cloud. Falls back to Google OAuth token if omitted.", styles['TableCell'])
        ],
        [
            Paragraph("<code>OLLAMA_HOST</code>", styles['TableCellCode']),
            Paragraph("<code>http://127.0.0.1:11434</code>", styles['TableCell']),
            Paragraph("Endpoint URL for local Ollama inference daemon.", styles['TableCell'])
        ],
        [
            Paragraph("<code>HGB_LOG</code>", styles['TableCellCode']),
            Paragraph("<code>info</code>", styles['TableCell']),
            Paragraph("Daemon logging verbosity (`trace`, `debug`, `info`, `warn`, `error`).", styles['TableCell'])
        ],
        [
            Paragraph("<code>HGB_CONFIG_DIR</code>", styles['TableCellCode']),
            Paragraph("<code>~/.hgb</code>", styles['TableCell']),
            Paragraph("Directory storing persistent SQLite memory ledger, style vault, and recipe runbooks.", styles['TableCell'])
        ],
        [
            Paragraph("<code>HGB_NO_COLOR</code>", styles['TableCellCode']),
            Paragraph("<code>0</code>", styles['TableCell']),
            Paragraph("Set to `1` to disable ANSI color codes in terminal output for plain-text logging.", styles['TableCell'])
        ]
    ]

    env_table = Table(env_data, colWidths=[130, 150, 252])
    env_table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), PRIMARY_NAVY),
        ('BOX', (0, 0), (-1, -1), 0.75, DARK_SLATE),
        ('INNERGRID', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, BG_ICE]),
        ('TOPPADDING', (0, 0), (-1, -1), 3),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 3),
        ('LEFTPADDING', (0, 0), (-1, -1), 5),
        ('RIGHTPADDING', (0, 0), (-1, -1), 5),
    ]))
    story.append(env_table)
    story.append(Spacer(1, 6))

    story.append(Paragraph("7.2 Process Exit Codes Reference", styles['SectionHeading']))
    story.append(Paragraph(
        "For CI/CD pipelines and shell scripting, Hagibis returns deterministic process exit codes:",
        styles['Body']
    ))
    story.append(Spacer(1, 3))

    exit_data = [
        [Paragraph("<b>Code</b>", styles['TableHeader']), Paragraph("<b>Identifier</b>", styles['TableHeader']), Paragraph("<b>Meaning &amp; Recommended Action</b>", styles['TableHeader'])],
        [Paragraph("<code>0</code>", styles['TableCellBold']), Paragraph("<code>SUCCESS</code>", styles['TableCellCode']), Paragraph("Operation completed successfully with all invariants green.", styles['TableCell'])],
        [Paragraph("<code>1</code>", styles['TableCellBold']), Paragraph("<code>GENERAL_ERROR</code>", styles['TableCellCode']), Paragraph("General syntax or runtime error occurred; check compiler logs.", styles['TableCell'])],
        [Paragraph("<code>2</code>", styles['TableCellBold']), Paragraph("<code>INVARIANT_FAILED</code>", styles['TableCellCode']), Paragraph("Verification Gate invariant failed; patch was safely rejected.", styles['TableCell'])],
        [Paragraph("<code>3</code>", styles['TableCellBold']), Paragraph("<code>SECRET_LEAK_BLOCKED</code>", styles['TableCellCode']), Paragraph("Pre-flight check intercepted potential secret or private key leak.", styles['TableCell'])],
        [Paragraph("<code>4</code>", styles['TableCellBold']), Paragraph("<code>IPC_TIMEOUT</code>", styles['TableCellCode']), Paragraph("Daemon IPC socket did not respond within configured timeout limit.", styles['TableCell'])],
        [Paragraph("<code>5</code>", styles['TableCellBold']), Paragraph("<code>BUDGET_BREACHED</code>", styles['TableCellCode']), Paragraph("FinOps monthly token budget exceeded; hard circuit breaker tripped.", styles['TableCell'])],
    ]
    exit_table = Table(exit_data, colWidths=[40, 150, 342])
    exit_table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), PRIMARY_NAVY),
        ('BOX', (0, 0), (-1, -1), 0.75, DARK_SLATE),
        ('INNERGRID', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, BG_ICE]),
        ('TOPPADDING', (0, 0), (-1, -1), 3),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 3),
        ('LEFTPADDING', (0, 0), (-1, -1), 5),
        ('RIGHTPADDING', (0, 0), (-1, -1), 5),
    ]))
    story.append(exit_table)
    story.append(Spacer(1, 8))

    # =========================================================================
    # THE VIBE CODER'S OATH & FLIGHT BENEDICTION BY TALARIA
    # =========================================================================
    story.append(Paragraph("7.3 The Vibe Coder's Oath &amp; Talaria's Flight Benediction", styles['SectionHeading']))
    story.append(Paragraph(
        "<i>'You have now traversed the sovereign architecture of Hagibis. You hold in your hands the tools "
        "to outpace friction, build at the speed of thought, and command swarms of verified AI intelligences.'</i>",
        styles['BodyBold']
    ))
    story.append(Spacer(1, 4))

    oath_text = (
        "<b>THE VIBE CODER'S SACRED OATH:</b><br/><br/>"
        "1. <b>I shall never suffer friction:</b> If a tool makes me wait, I shall replace it with sub-millisecond systems.<br/>"
        "2. <b>I shall never fear experimentation:</b> Because my state is safeguarded by sub-10µs atomic rollbacks and WAL checkpoints.<br/>"
        "3. <b>I shall never accept hallucinations:</b> Because code must be verified by the compiler and golden invariants before touching disk.<br/>"
        "4. <b>I shall remain sovereign:</b> I shall run offline local models with zero cost, calling frontier cloud reasoning only when necessary.<br/>"
        "5. <b>I shall build with joy:</b> Because coding is not mechanical toil—it is pure, uninhibited creative architecture.<br/><br/>"
        "<i>Put on the winged golden sandals of Talaria. Traverse the cosmos of code. Join the velocity revolution.</i>"
    )

    story.append(make_callout("FINAL FLIGHT BENEDICTION BY TALARIA", oath_text, kind="wisdom", width=532))

    return story
