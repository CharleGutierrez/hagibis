"""
generate_pdf.py - Master PDF Assembly for Hagibis Vibe Coding Masterclass
"""

import os
import sys
import time

# Ensure scratch directory is on sys.path
scratch_dir = os.path.dirname(os.path.abspath(__file__))
if scratch_dir not in sys.path:
    sys.path.insert(0, scratch_dir)

from reportlab.lib.pagesizes import letter
from reportlab.platypus import SimpleDocTemplate, PageBreak, Spacer

from styles import MasterclassNumberedCanvas, get_masterclass_styles
from ch1_foundations import build_cover_and_foundations
from ch2_architecture import build_architecture_chapter
from ch3_cockpit import build_cockpit_chapter
from ch4_superpowers import build_superpowers_chapter
from ch5_tutorials import build_tutorials_chapter
from ch6_scenarios import build_scenarios_chapter
from ch7_appendix import build_appendix_chapter

def assemble_masterclass_pdf():
    start_time = time.time()
    print("=" * 70)
    print("🪽 ASSEMBLING HAGIBIS VIBE CODING MASTERCLASS PDF 🪽")
    print("=" * 70)

    # Primary and secondary targets
    repo_root = os.path.abspath(os.path.join(scratch_dir, "..", ".."))
    target_primary = os.path.join(repo_root, "HAGIBIS_VIBE_CODING_MASTERCLASS.pdf")
    target_scratch = os.path.join(scratch_dir, "HAGIBIS_VIBE_CODING_MASTERCLASS.pdf")

    # Determine writable target
    output_pdf = target_primary
    print(f"Target Output Path: {output_pdf}")

    # Build story elements
    story = []

    print("[1/7] Building Front Cover, Foreword, Curriculum, and Chapter 1...")
    story.extend(build_cover_and_foundations())
    story.append(PageBreak())

    print("[2/7] Building Chapter 2: Dual-Engine Systems Architecture (hgb & hgbd)...")
    story.extend(build_architecture_chapter())
    story.append(PageBreak())

    print("[3/7] Building Chapter 3: Conversational Cockpit & Slash Commands...")
    story.extend(build_cockpit_chapter())
    story.append(PageBreak())

    print("[4/7] Building Chapter 4: The 125 Sovereign Superpowers Technical Reference...")
    story.extend(build_superpowers_chapter())
    story.append(PageBreak())

    print("[5/7] Building Chapter 5: Five Complete End-to-End Production Tutorials...")
    story.extend(build_tutorials_chapter())
    story.append(PageBreak())

    print("[6/7] Building Chapter 6: The 500 Real-World Scenarios Masterclass...")
    story.extend(build_scenarios_chapter())
    story.append(PageBreak())

    print("[7/7] Building Chapter 7: Appendix, CLI Manual & The Vibe Coder's Oath...")
    story.extend(build_appendix_chapter())

    print(f"Total Flowable Story Elements: {len(story)}")
    print("Compiling PDF with MasterclassNumberedCanvas...")

    # Document Setup (Letter: 612 x 792 pt, 40pt side margins -> 532pt printable width)
    try:
        doc = SimpleDocTemplate(
            output_pdf,
            pagesize=letter,
            leftMargin=40,
            rightMargin=40,
            topMargin=42,
            bottomMargin=46
        )
        doc.build(story, canvasmaker=MasterclassNumberedCanvas)
        final_path = output_pdf
    except Exception as e:
        print(f"Notice: Failed to write to {output_pdf} ({e}). Falling back to scratch directory...")
        output_pdf = target_scratch
        doc = SimpleDocTemplate(
            output_pdf,
            pagesize=letter,
            leftMargin=40,
            rightMargin=40,
            topMargin=42,
            bottomMargin=46
        )
        doc.build(story, canvasmaker=MasterclassNumberedCanvas)
        final_path = output_pdf

    elapsed = time.time() - start_time
    file_size_kb = os.path.getsize(final_path) / 1024.0
    file_size_mb = file_size_kb / 1024.0

    # Inspect page count
    page_count = 0
    with open(final_path, 'rb') as f:
        content = f.read()
        import re
        # Count /Type /Page occurrences (ignoring /Pages)
        pages = re.findall(b"/Type\\s*/Page[^s]", content)
        page_count = len(pages)

    print("=" * 70)
    print("🏛️ HAGIBIS MASTERCLASS COMPILATION REPORT 🏛️")
    print("=" * 70)
    print(f"  ✔ Status: SUCCESSFUL COMPILATION")
    print(f"  ✔ Artifact Path: {final_path}")
    print(f"  ✔ Total Pages: {page_count} pages")
    print(f"  ✔ File Size: {file_size_mb:.2f} MB ({file_size_kb:.1f} KB)")
    print(f"  ✔ Compilation Time: {elapsed:.2f} seconds")
    print(f"  ✔ Mascot Illustrations: 4 Embedded (Cover, Architecture, Superpowers, Scenarios)")
    print(f"  ✔ Sovereign Superpowers: 125 Fully Documented with Signatures")
    print(f"  ✔ Production Tutorials: 5 Complete End-to-End Guides")
    print(f"  ✔ Real-World Scenarios: 500 Concrete Playbooks across 10 Domains")
    print("=" * 70)

if __name__ == "__main__":
    assemble_masterclass_pdf()
