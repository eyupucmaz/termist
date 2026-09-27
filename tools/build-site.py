#!/usr/bin/env python3
"""Builds docs/index.html, the GitHub Pages site.

The hero is the Galata scene from assets/scenes, rendered once per time of day with the
same palettes and effects the terminal renderer uses; a few lines of inline script show the
one that matches the visitor's clock. Run it after editing the scene or the page:

    python3 tools/build-site.py
"""
import html
import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("scene_preview", ROOT / "tools" / "scene-preview.py")
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)

TIMES = ["sabah", "gunduz", "aksam", "gece"]


def hexc(rgb):
    return "#%02x%02x%02x" % tuple(rgb)


def scene_pre(scene, tod):
    rows = []
    for row in scene.frame(tod, 0):
        out, run, style = [], [], None
        for ch, fg, bg in row:
            s = (hexc(fg), hexc(bg))
            if s != style and run:
                out.append(f'<span style="color:{style[0]};background:{style[1]}">{html.escape("".join(run))}</span>')
                run = []
            style = s
            run.append(ch)
        if run:
            out.append(f'<span style="color:{style[0]};background:{style[1]}">{html.escape("".join(run))}</span>')
        rows.append("".join(out))
    return f'<pre class="scene" data-tod="{tod}" aria-hidden="true">' + "\n".join(rows) + "</pre>"


def band(scene, tod):
    """The sky and water beside the scene: each row's edge colour, as hard gradient stops."""
    rows = scene.frame(tod, 0)
    stops = []
    for i, row in enumerate(rows):
        c = hexc(row[0][2])
        stops.append(f"{c} {i * 100 / len(rows):.3f}% {(i + 1) * 100 / len(rows):.3f}%")
    selector = f':root[data-tod="{tod}"] .sky'
    if tod == "gunduz":  # also without script
        selector += ", :root:not([data-tod]) .sky"
    return f'{selector} {{ background: linear-gradient({", ".join(stops)}); }}'


def main():
    scene = preview.Scene("galata")
    bands = "\n".join(band(scene, t) for t in TIMES)
    scenes = f"<style>\n{bands}\n</style>\n" + "\n".join(scene_pre(scene, t) for t in TIMES)
    template = (ROOT / "docs" / "site.html").read_text()
    out = template.replace("<!-- SCENES -->", scenes)
    (ROOT / "docs" / "index.html").write_text(out)
    print(f"docs/index.html: {len(out) // 1024} KiB")


if __name__ == "__main__":
    main()
