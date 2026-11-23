# -*- coding: utf-8 -*-
import sys
from pptx import Presentation
from pptx.enum.shapes import MSO_SHAPE_TYPE

path = sys.argv[1]
prs = Presentation(path)

def safe(fn, default=False):
    try:
        return fn()
    except Exception:
        return default

def walk_shapes(shapes, out, indent=""):
    for shape in shapes:
        # Group shapes: recurse (detect by tag to avoid shape_type raising)
        tag = shape.element.tag.split("}")[-1]
        if tag == "grpSp":
            walk_shapes(shape.shapes, out, indent)
            continue
        # Text frame
        if safe(lambda: shape.has_text_frame):
            txt = shape.text_frame.text
            if txt.strip():
                out.append(txt.strip())
        # Table
        if safe(lambda: shape.has_table):
            for row in shape.table.rows:
                cells = [c.text.strip() for c in row.cells]
                out.append(" | ".join(cells))
        # Chart: categories / series names
        if safe(lambda: shape.has_chart):
            try:
                ch = shape.chart
                for s in ch.series:
                    if s.name:
                        out.append("[chart series] " + str(s.name))
                try:
                    cats = list(ch.plots[0].categories)
                    out.append("[chart categories] " + " | ".join(str(c) for c in cats))
                except Exception:
                    pass
            except Exception:
                pass

lines = []
for i, slide in enumerate(prs.slides, 1):
    lines.append("")
    lines.append("=" * 60)
    lines.append("SLIDE %d" % i)
    lines.append("=" * 60)
    out = []
    walk_shapes(slide.shapes, out)
    for t in out:
        lines.append(t)
    # Speaker notes
    if slide.has_notes_slide:
        notes = slide.notes_slide.notes_text_frame.text
        if notes.strip():
            lines.append("[NOTES] " + notes.strip())

print("\n".join(lines))
