#!/usr/bin/env python3
"""Builds the README's banner: the Galata scene with the name beside it.

One banner per GitHub theme: the scene by day for the light one, at dusk for the dark one.
Each is laid out as a small HTML page and photographed at twice its size by headless Chrome:

    python3 tools/build-banner.py
"""
import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("build_site", ROOT / "tools" / "build-site.py")
site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(site)

CHROME = {
    "darwin": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "linux": "google-chrome",
}
WIDTH, HEIGHT = 1280, 400
# the time of day, and the ink that reads on its sky
BANNERS = {
    "light": ("gunduz", "#13263b", "#2c4a66"),
    "dark": ("aksam", "#ffffff", "#f3dcc8"),
}

PAGE = """<!doctype html>
<html><head><meta charset="utf-8">
<link href="https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,400;12..96,800&family=JetBrains+Mono&display=block" rel="stylesheet">
<style>
  {band}
  html, body {{ margin: 0; width: {width}px; height: {height}px; overflow: hidden; }}
  .sky {{ position: relative; width: 100%; height: 100%; }}
  .scene {{
    position: absolute; left: 0; top: 0; margin: 0;
    font: 400 12.8px/{line}px "JetBrains Mono", monospace; letter-spacing: 0;
    /* the scene's right edge melts into the sky and the water beside it */
    -webkit-mask-image: linear-gradient(to right, #000 calc(100% - 90px), transparent);
    mask-image: linear-gradient(to right, #000 calc(100% - 90px), transparent);
  }}
  .words {{
    position: absolute; right: 64px; top: 58px; text-align: right;
    font-family: "Bricolage Grotesque", sans-serif; color: {ink};
  }}
  .name {{ margin: 0; font-size: 104px; font-weight: 800; line-height: 0.9; letter-spacing: -0.045em; font-stretch: 85%; }}
  .gloss {{ margin: 8px 0 0; font-size: 22px; color: {soft}; }}
  .pitch {{ margin: 22px 0 0; font-size: 27px; line-height: 1.25; }}
</style></head>
<body><div class="sky">{scene}
<div class="words">
  <p class="name">termist</p>
  <p class="gloss">terminal istanbul</p>
  <p class="pitch">mission control<br>for your coding agents</p>
</div></div></body></html>
"""


def main():
    chrome = CHROME.get(sys.platform)
    if not chrome:
        sys.exit("build-banner: needs Google Chrome (macOS or Linux)")
    scene = site.preview.Scene("galata")
    rows = len(scene.frame("aksam", 0))
    out_dir = ROOT / "assets" / "readme"
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        for theme, (tod, ink, soft) in BANNERS.items():
            band = site.band(scene, tod).split(" {", 1)[1]
            page = PAGE.format(
                band=".sky {" + band,
                width=WIDTH,
                height=HEIGHT,
                line=HEIGHT / rows,
                ink=ink,
                soft=soft,
                scene=site.scene_pre(scene, tod),
            )
            src = Path(tmp) / f"{theme}.html"
            src.write_text(page)
            png = out_dir / f"banner-{theme}.png"
            subprocess.run(
                [chrome, "--headless=new", "--hide-scrollbars", "--force-device-scale-factor=2",
                 f"--window-size={WIDTH},{HEIGHT}", "--virtual-time-budget=5000",
                 f"--screenshot={png}", src.as_uri()],
                check=True, capture_output=True,
            )
            print(f"{png.relative_to(ROOT)}: {png.stat().st_size // 1024} KiB")


if __name__ == "__main__":
    main()
