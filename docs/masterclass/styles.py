"""
styles.py - Visual Design System and PDF Canvas for the Hagibis Masterclass
"""

from reportlab.lib import colors
from reportlab.lib.styles import getSampleStyleSheet, ParagraphStyle
from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, HRFlowable
from reportlab.pdfgen import canvas

# =========================================================================
# COLOR PALETTE
# =========================================================================
PRIMARY_NAVY = colors.HexColor("#0f172a")     # Slate 900
DARK_SLATE = colors.HexColor("#1e293b")       # Slate 800
MID_SLATE = colors.HexColor("#334155")        # Slate 700
MUTED_SLATE = colors.HexColor("#64748b")      # Slate 500
LIGHT_BORDER = colors.HexColor("#cbd5e1")     # Slate 300
BG_ICE = colors.HexColor("#f8fafc")           # Slate 50
BG_CARD = colors.HexColor("#f1f5f9")          # Slate 100

CYAN_ACCENT = colors.HexColor("#06b6d4")      # Cyan 500
CYAN_DARK = colors.HexColor("#0891b2")        # Cyan 600
CYAN_LIGHT = colors.HexColor("#ecfeff")       # Cyan 50

PURPLE_ACCENT = colors.HexColor("#8b5cf6")    # Violet 500
PURPLE_LIGHT = colors.HexColor("#f5f3ff")     # Violet 50

EMERALD_GREEN = colors.HexColor("#10b981")    # Emerald 500
EMERALD_LIGHT = colors.HexColor("#ecfdf5")    # Emerald 50

AMBER_GOLD = colors.HexColor("#f59e0b")       # Amber 500
AMBER_LIGHT = colors.HexColor("#fffbeb")      # Amber 50

CODE_BG = colors.HexColor("#090d16")          # Pitch Dark Navy
CODE_TEXT = colors.HexColor("#38bdf8")        # Sky 400

# =========================================================================
# NUMBERED CANVAS WITH RUNNING HEADERS & FOOTERS
# =========================================================================
class MasterclassNumberedCanvas(canvas.Canvas):
    """
    Two-pass canvas that computes total pages dynamically and renders
    running headers and page footers ('Page X of Y') on all pages except
    the front cover.
    """
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._saved_page_states = []

    def showPage(self):
        self._saved_page_states.append(dict(self.__dict__))
        self._startPage()

    def save(self):
        num_pages = len(self._saved_page_states)
        for state in self._saved_page_states:
            self.__dict__.update(state)
            if self._pageNumber > 1:
                self.saveState()
                
                # Header (Top)
                self.setFont("Helvetica-Bold", 7.5)
                self.setFillColor(MUTED_SLATE)
                self.drawString(40, 762, "🪽 HAGIBIS (hgb / hgbd) — THE DEFINITIVE VIBE CODING MASTERCLASS")
                self.setFont("Helvetica", 7.5)
                self.drawRightString(572, 762, "TALARIA SOVEREIGN MANUAL")
                
                # Top Accent Rule
                self.setStrokeColor(LIGHT_BORDER)
                self.setLineWidth(0.5)
                self.line(40, 756, 572, 756)
                
                # Bottom Accent Rule
                self.line(40, 42, 572, 42)
                
                # Footer (Bottom)
                self.setFont("Helvetica", 7.5)
                self.setFillColor(MUTED_SLATE)
                self.drawString(40, 30, "Sub-Millisecond Microkernel & Swarm Engine • 87 Sovereign Superpowers • 500 Scenarios")
                
                page_str = f"Page {self._pageNumber} of {num_pages}"
                self.setFont("Helvetica-Bold", 7.5)
                self.setFillColor(PRIMARY_NAVY)
                self.drawRightString(572, 30, page_str)
                
                self.restoreState()
            canvas.Canvas.showPage(self)
        canvas.Canvas.save(self)


# =========================================================================
# PARAGRAPH STYLES
# =========================================================================
def get_masterclass_styles():
    base = getSampleStyleSheet()
    styles = {}
    
    # Titles & Headings
    styles['CoverTitle'] = ParagraphStyle(
        'CoverTitle',
        parent=base['Title'],
        fontName='Helvetica-Bold',
        fontSize=24,
        leading=28,
        textColor=PRIMARY_NAVY,
        alignment=1,
        spaceAfter=6
    )
    
    styles['CoverSubtitle'] = ParagraphStyle(
        'CoverSubtitle',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=11,
        leading=15,
        textColor=DARK_SLATE,
        alignment=1,
        spaceAfter=12
    )
    
    styles['ChapterHeading'] = ParagraphStyle(
        'ChapterHeading',
        parent=base['Heading1'],
        fontName='Helvetica-Bold',
        fontSize=17,
        leading=21,
        textColor=PRIMARY_NAVY,
        spaceBefore=14,
        spaceAfter=7,
        keepWithNext=True
    )
    
    styles['SectionHeading'] = ParagraphStyle(
        'SectionHeading',
        parent=base['Heading2'],
        fontName='Helvetica-Bold',
        fontSize=12,
        leading=15,
        textColor=CYAN_DARK,
        spaceBefore=11,
        spaceAfter=5,
        keepWithNext=True
    )
    
    styles['SubSectionHeading'] = ParagraphStyle(
        'SubSectionHeading',
        parent=base['Heading3'],
        fontName='Helvetica-Bold',
        fontSize=9.5,
        leading=12.5,
        textColor=DARK_SLATE,
        spaceBefore=8,
        spaceAfter=3,
        keepWithNext=True
    )

    styles['SuperpowerTitle'] = ParagraphStyle(
        'SuperpowerTitle',
        parent=base['Heading3'],
        fontName='Helvetica-Bold',
        fontSize=9.2,
        leading=12,
        textColor=PRIMARY_NAVY,
        spaceBefore=7,
        spaceAfter=2,
        keepWithNext=True
    )
    
    # Body & Flow
    styles['Body'] = ParagraphStyle(
        'Body',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=8.2,
        leading=11.2,
        textColor=DARK_SLATE,
        spaceAfter=4
    )

    styles['BodyBold'] = ParagraphStyle(
        'BodyBold',
        parent=base['Normal'],
        fontName='Helvetica-Bold',
        fontSize=8.2,
        leading=11.2,
        textColor=PRIMARY_NAVY,
        spaceAfter=4
    )

    styles['BulletText'] = ParagraphStyle(
        'BulletText',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=8,
        leading=11,
        textColor=DARK_SLATE,
        leftIndent=12,
        firstLineIndent=-8,
        spaceAfter=2.5
    )

    # Callouts & Notes
    styles['CalloutText'] = ParagraphStyle(
        'CalloutText',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=7.8,
        leading=10.5,
        textColor=DARK_SLATE
    )

    styles['CalloutTitle'] = ParagraphStyle(
        'CalloutTitle',
        parent=base['Normal'],
        fontName='Helvetica-Bold',
        fontSize=8.2,
        leading=11,
        textColor=PRIMARY_NAVY,
        spaceAfter=2
    )

    # Code & Monospace
    styles['CodeInline'] = ParagraphStyle(
        'CodeInline',
        parent=base['Normal'],
        fontName='Courier-Bold',
        fontSize=7.8,
        leading=9.8,
        textColor=CYAN_DARK
    )

    styles['CodeBlock'] = ParagraphStyle(
        'CodeBlock',
        parent=base['Normal'],
        fontName='Courier',
        fontSize=7,
        leading=9.2,
        textColor=CODE_TEXT
    )

    # Table Cells
    styles['TableHeader'] = ParagraphStyle(
        'TableHeader',
        parent=base['Normal'],
        fontName='Helvetica-Bold',
        fontSize=7.5,
        leading=9.5,
        textColor=colors.white,
        alignment=0
    )

    styles['TableCell'] = ParagraphStyle(
        'TableCell',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=7.2,
        leading=9.5,
        textColor=DARK_SLATE
    )

    styles['TableCellBold'] = ParagraphStyle(
        'TableCellBold',
        parent=base['Normal'],
        fontName='Helvetica-Bold',
        fontSize=7.2,
        leading=9.5,
        textColor=PRIMARY_NAVY
    )

    styles['TableCellCode'] = ParagraphStyle(
        'TableCellCode',
        parent=base['Normal'],
        fontName='Courier',
        fontSize=6.8,
        leading=8.8,
        textColor=CYAN_DARK
    )

    # Scenario Elements
    styles['ScenarioHeader'] = ParagraphStyle(
        'ScenarioHeader',
        parent=base['Normal'],
        fontName='Helvetica-Bold',
        fontSize=8,
        leading=10.5,
        textColor=PRIMARY_NAVY
    )

    styles['ScenarioMeta'] = ParagraphStyle(
        'ScenarioMeta',
        parent=base['Normal'],
        fontName='Helvetica-Oblique',
        fontSize=7,
        leading=9.2,
        textColor=MUTED_SLATE
    )

    styles['ScenarioCommand'] = ParagraphStyle(
        'ScenarioCommand',
        parent=base['Normal'],
        fontName='Courier-Bold',
        fontSize=6.8,
        leading=8.8,
        textColor=CODE_TEXT
    )

    styles['ScenarioOutcome'] = ParagraphStyle(
        'ScenarioOutcome',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=7,
        leading=9.2,
        textColor=DARK_SLATE
    )

    styles['ScenarioTip'] = ParagraphStyle(
        'ScenarioTip',
        parent=base['Normal'],
        fontName='Helvetica',
        fontSize=6.8,
        leading=9,
        textColor=colors.HexColor("#065f46")
    )

    return styles


# =========================================================================
# HELPER UI FLOWABLE GENERATORS
# =========================================================================

def make_callout(title, text, kind="tip", width=532):
    styles = get_masterclass_styles()
    bg_map = {
        "tip": EMERALD_LIGHT,
        "note": CYAN_LIGHT,
        "warning": AMBER_LIGHT,
        "wisdom": PURPLE_LIGHT
    }
    border_map = {
        "tip": EMERALD_GREEN,
        "note": CYAN_ACCENT,
        "warning": AMBER_GOLD,
        "wisdom": PURPLE_ACCENT
    }
    icon_map = {
        "tip": "🪽 TALARIA SWIFT TIP",
        "note": "ℹ️ ARCHITECTURE INSIGHT",
        "warning": "⚠️ VIBE CODING PITFALL",
        "wisdom": "⚡ TALARIA SOVEREIGN FLIGHT WISDOM"
    }
    
    bg_col = bg_map.get(kind, CYAN_LIGHT)
    border_col = border_map.get(kind, CYAN_ACCENT)
    icon_header = icon_map.get(kind, "NOTE")
    full_title = f"{icon_header}: {title}" if title else icon_header
    
    content = [
        Paragraph(f"<b>{full_title}</b>", styles['CalloutTitle']),
        Paragraph(text, styles['CalloutText'])
    ]
    
    table = Table([[content]], colWidths=[width])
    table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, -1), bg_col),
        ('BOX', (0, 0), (-1, -1), 0.5, LIGHT_BORDER),
        ('LINELEFT', (0, 0), (-1, -1), 3.0, border_col),
        ('TOPPADDING', (0, 0), (-1, -1), 4),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 5),
        ('LEFTPADDING', (0, 0), (-1, -1), 8),
        ('RIGHTPADDING', (0, 0), (-1, -1), 8),
    ]))
    return table


def make_code_block(code_text, caption=None, width=532):
    styles = get_masterclass_styles()
    lines = code_text.strip().split('\n')
    escaped_lines = [
        line.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;').replace(' ', '&nbsp;')
        for line in lines
    ]
    formatted_html = "<br/>".join(escaped_lines)
    p = Paragraph(formatted_html, styles['CodeBlock'])
    
    elements = []
    if caption:
        cap_p = Paragraph(f"<b>CLI Execution:</b> <i>{caption}</i>", styles['TableCellBold'])
        elements.append(cap_p)
        elements.append(Spacer(1, 2))
    elements.append(p)
    
    table = Table([[elements]], colWidths=[width])
    table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, -1), CODE_BG),
        ('BOX', (0, 0), (-1, -1), 0.5, DARK_SLATE),
        ('LINELEFT', (0, 0), (-1, -1), 2.5, CYAN_ACCENT),
        ('TOPPADDING', (0, 0), (-1, -1), 3),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 3),
        ('LEFTPADDING', (0, 0), (-1, -1), 6),
        ('RIGHTPADDING', (0, 0), (-1, -1), 6),
    ]))
    return table


def make_divider(color=LIGHT_BORDER, thickness=0.5, space_before=4, space_after=6):
    return HRFlowable(
        width="100%",
        thickness=thickness,
        color=color,
        spaceBefore=space_before,
        spaceAfter=space_after
    )
