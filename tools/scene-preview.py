#!/usr/bin/env python3
"""Preview termist's Istanbul scenes in a truecolor terminal.

Authoring aid only: the shipped renderer lives in Rust. This script reads the same
assets the app will (see assets/scenes/README.md), so what you approve here is
what the Rust code must reproduce.

  python3 tools/scene-preview.py galata              # time of day from the clock
  python3 tools/scene-preview.py galata --time gece  # sabah | gunduz | aksam | gece
  python3 tools/scene-preview.py galata --cycle      # walk through all four palettes
  python3 tools/scene-preview.py galata --plain      # print the raw art once, no colour
  python3 tools/scene-preview.py galata --check      # validate assets and render frames headless
  python3 tools/scene-preview.py galata --png out.png [--frame 9]   # all four palettes, stacked

Keys: q or Esc quits, space pauses, t steps to the next palette.
"""
import argparse
import datetime
import math
import os
import random
import select
import shutil
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "assets" / "scenes"
TIMES = ["sabah", "gunduz", "aksam", "gece"]
LABELS = {"sabah": "sabah", "gunduz": "gündüz", "aksam": "gün batımı", "gece": "gece"}
FPS = 10


def time_of_day(now=None):
    h = (now or datetime.datetime.now()).hour
    if 6 <= h < 11:
        return "sabah"
    if 11 <= h < 17:
        return "gunduz"
    if 17 <= h < 20:
        return "aksam"
    return "gece"


def hex_rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def gradient(stops, t):
    stops = [hex_rgb(s) for s in stops]
    if len(stops) == 1:
        return stops[0]
    t = min(max(t, 0.0), 1.0) * (len(stops) - 1)
    i = min(int(t), len(stops) - 2)
    f = t - i
    a, b = stops[i], stops[i + 1]
    return tuple(round(a[k] + (b[k] - a[k]) * f) for k in range(3))


def pad(lines, w):
    return [r.ljust(w) for r in lines]


def load_palettes(scene_dir):
    """Shared palettes.toml, overridden per time of day by the scene's own palette.toml."""
    base = tomllib.loads((ROOT / "palettes.toml").read_text())
    own_file = scene_dir / "palette.toml"
    own = tomllib.loads(own_file.read_text()) if own_file.exists() else {}
    return {t: {**base.get(t, {}), **own.get(t, {})} for t in TIMES}


def active(effect, tod):
    times = effect.get("times")
    return times is None or tod in times


def stable_seed(text):
    return sum((i + 1) * ord(c) for i, c in enumerate(text))


# --------------------------------------------------------------------------- effects
# Each effect mutates `cells` (rows of [char, fg, bg]) for frame n. The vocabulary is
# deliberately small; the Rust renderer implements exactly these.

def fx_waves(sc, e, cells, pal, tod, n):
    """Cells of mask `key` are water: bg gradient `water`, the art's own '~' pattern
    scrolls sideways, alternate rows in opposite directions; optional glints whose
    density per column stays constant (`glint_density` scales it)."""
    key = e.get("key", "w")
    every = e.get("every", 2)
    rows = [y for y in range(sc.h) if key in sc.mask[y]]
    if not rows:
        return
    top, bottom = min(rows), max(rows)
    # keep glints per column constant however tall the water is (calibrated on Galata's 4 rows)
    scale = min(1.0, e.get("glint_rows", 4) / len(rows)) * e.get("glint_density", 1.0)
    star_t, dot_t = math.cos(math.acos(0.993) * scale), math.cos(math.acos(0.975) * scale)
    for y in rows:
        pattern = "".join(a if m == key else " " for a, m in zip(sc.art[y], sc.mask[y]))
        offset = (n // every) * (1 if y % 2 else -1)
        bg = gradient(pal["water"], (y - top) / max(bottom - top, 1))
        for x in range(sc.w):
            if sc.mask[y][x] != key:
                continue
            ch = pattern[(x + offset) % sc.w]
            fg = hex_rgb(pal["wave"])
            if ch != "~":
                ch = " "
                if e.get("glints", True):
                    g = math.sin(x * 12.9898 + y * 78.233 + n * 0.35)
                    if g > star_t:
                        ch, fg = "*", (255, 255, 240)
                    elif g > dot_t:
                        ch = "."
            cells[y][x] = [ch, fg, bg]


def fx_stars(sc, e, cells, pal, tod, n):
    for y, x, ch, ph in sc.star_cells(e.get("count", 28)):
        if math.sin(n / 7 + ph) > -0.3:
            cells[y][x][0], cells[y][x][1] = ch, (220, 225, 245)


def fx_gulls(sc, e, cells, pal, tod, n):
    """Seagulls fly right to left across sky cells, flapping."""
    colour = hex_rgb(e.get("color", "#fffaf5"))
    for i, row in enumerate(e.get("rows", [4, 6, 3])):
        speed, phase = (1.1, 0.8, 0.65, 0.95)[i % 4], i * 30
        x = sc.w - 1 - int((n * speed + phase * 3) % (sc.w + 6))
        y = row + round(math.sin((n + phase) / 9))
        sprite = "\\v/" if (n // 3 + i) % 2 else "-v-"
        for k, ch in enumerate(sprite):
            cx = x + k
            if 0 <= cx < sc.w and 0 <= y < sc.h and sc.mask[y][cx] == " ":
                cells[y][cx][0], cells[y][cx][1] = ch, colour


def draw_puffs(sc, cells, tod, n, cy, cx, drift):
    """Five staggered puffs rising from (cy, cx), drifting sideways, over sky cells only."""
    for k in range(5):
        age = (n + k * 8) % 40
        py, px = cy - 1 - age // 8, cx + drift * (age // 5)
        ch = "o" if age < 10 else ("O" if age < 24 else ".")
        if 0 <= py < sc.h and 0 <= px < sc.w and sc.mask[py][px] == " ":
            shade = 235 - age * 3 if tod != "gece" else 120 - age
            cells[py][px][0], cells[py][px][1] = ch, (shade, shade, shade)


def fx_smoke(sc, e, cells, pal, tod, n):
    """Puffs rise from the top of the cells marked `key` and drift sideways."""
    src = [(y, x) for y in range(sc.h) for x in range(sc.w) if sc.mask[y][x] == e.get("key", "F")]
    if src:
        draw_puffs(sc, cells, tod, n, min(src)[0], sum(x for _, x in src) // len(src), e.get("drift", -1))


def fx_twinkle(sc, e, cells, pal, tod, n):
    """Cells of mask `key` (windows, lamps) go dark now and then."""
    key = e.get("key", "W")
    off = hex_rgb(e.get("off", pal.get("D", {}).get("fg", "#333333")))
    for (y, x), seed in sc.seeds(key).items():
        if math.sin(n / 40 + seed * 50) > e.get("threshold", 0.85):
            cells[y][x][1] = off


def fx_blink(sc, e, cells, pal, tod, n):
    """Cells of mask `key` cycle through `colors`; `wave` shifts the phase along x."""
    colours = [hex_rgb(c) for c in e["colors"]]
    period = e.get("period", 20)
    for y in range(sc.h):
        for x in range(sc.w):
            if sc.mask[y][x] == e["key"]:
                i = int((n + x * e.get("wave", 0)) // period) % len(colours)
                cells[y][x][1] = colours[i]


def fx_sprite(sc, e, cells, pal, tod, n):
    """A multi-line object moving horizontally and wrapping around. Its mask chars are
    palette keys ('.' = transparent). Drawn only over cells whose scene mask is in `over`.
    `smoke_key`: sprite cells with this key emit chimney smoke that trails behind.
    `frames` (+ optional `mask_frames`, `frame_every=3`): flip-book animation instead of `art`.
    `twinkle_key` (+ `twinkle_times`, `twinkle_off`): those sprite cells go dark now and then."""
    i = (n // e.get("frame_every", 3)) % len(e["frames"]) if "frames" in e else 0
    art = e["frames"][i] if "frames" in e else e["art"]
    mask = e["mask_frames"][i] if "mask_frames" in e else e["mask"]
    w = max(max(len(r) for r in art), max(len(r) for r in mask))
    art, mask = pad(art, w), pad(mask, w)
    pos = int(n * e.get("speed", 0.3)) % (sc.w + w)
    x0 = pos - w if e.get("direction", 1) > 0 else sc.w - pos
    over = e.get("over", " w")
    if e.get("smoke_key"):  # a moving chimney smokes too, trailing behind the motion
        src = [(e["row"] + dy, x0 + dx) for dy, mr in enumerate(mask) for dx, k in enumerate(mr) if k == e["smoke_key"]]
        if src:
            draw_puffs(sc, cells, tod, n, min(src)[0], sum(x for _, x in src) // len(src),
                       -1 if e.get("direction", 1) > 0 else 1)
    for dy, (ar, mr) in enumerate(zip(art, mask)):
        y = e["row"] + dy
        for dx, (ch, k) in enumerate(zip(ar, mr)):
            x = x0 + dx
            if k == "." or not (0 <= x < sc.w and 0 <= y < sc.h) or sc.mask[y][x] not in over:
                continue
            spec = pal.get(k, {})
            fg = hex_rgb(spec["fg"]) if "fg" in spec else cells[y][x][1]
            bg = hex_rgb(spec["bg"]) if "bg" in spec else cells[y][x][2]
            if k == e.get("twinkle_key") and tod in e.get("twinkle_times", ["aksam", "gece"]):
                if math.sin(n / 40 + ((dx * 7919 + dy * 104729) % 1000) / 20) > 0.85:
                    fg = hex_rgb(e.get("twinkle_off", pal.get("D", {}).get("fg", "#333333")))
            cells[y][x] = [ch, fg, bg]


def fx_drip(sc, e, cells, pal, tod, n):
    """A drop falls down column `col` from row `top` to row `bottom`, then rings spread."""
    period = e.get("period", 40)
    t = (n + e.get("phase", 0)) % period
    col, top, bottom = e["col"], e["top"], e["bottom"]
    fall = bottom - top
    colour = hex_rgb(e.get("color", "#bfe6ff"))
    if t < fall:
        y = top + t
        if sc.mask[y][col] in e.get("over", " "):
            cells[y][col][0], cells[y][col][1] = "'", colour
    else:
        r = t - fall
        if r < 6:
            for dx in {-r * 2, r * 2}:
                x = col + dx
                if 0 <= x < sc.w:
                    cells[bottom][x][0] = "o" if r == 0 else ("(" if dx < 0 else ")")
                    cells[bottom][x][1] = colour


EFFECTS = {"waves": fx_waves, "stars": fx_stars, "gulls": fx_gulls, "smoke": fx_smoke,
           "twinkle": fx_twinkle, "blink": fx_blink, "sprite": fx_sprite, "drip": fx_drip}


class Scene:
    def __init__(self, name):
        self.name = name
        d = ROOT / name
        art = (d / "large.art.txt").read_text().rstrip("\n").split("\n")
        mask = (d / "large.mask.txt").read_text().rstrip("\n").split("\n")
        if len(art) != len(mask):
            raise SystemExit(f"{name}: art has {len(art)} rows, mask has {len(mask)}")
        self.w = max(max(len(r) for r in art), max(len(r) for r in mask))
        self.art, self.mask, self.h = pad(art, self.w), pad(mask, self.w), len(art)
        self.meta = tomllib.loads((d / "scene.toml").read_text())
        self.palettes = load_palettes(d)
        for e in self.meta.get("effects", []):
            if e["type"] not in EFFECTS:
                raise SystemExit(f"{name}: unknown effect type {e['type']!r}")
        # the sky gradient spans down to the first row containing the horizon key
        horizon_key = self.meta.get("horizon_key", "w")
        rows = [y for y, r in enumerate(self.mask) if horizon_key in r]
        self.horizon = min(rows) if rows else self.h
        # only a water horizon puts the water gradient behind uncoloured cells below it
        self.horizon_is_water = any(e["type"] == "waves" and e.get("key", "w") == horizon_key
                                    for e in self.meta.get("effects", []))
        self._seeds, self._stars = {}, None

    def seeds(self, key):
        if key not in self._seeds:
            rng = random.Random(stable_seed(key))
            self._seeds[key] = {(y, x): rng.random() for y in range(self.h) for x in range(self.w)
                                if self.mask[y][x] == key}
        return self._seeds[key]

    def star_cells(self, count):
        if self._stars is None:
            rng = random.Random(42)
            sky = [(y, x) for y in range(max(self.horizon - 6, 0)) for x in range(self.w)
                   if self.mask[y][x] == " "]
            self._stars = [(y, x, rng.choice(".·*+"), rng.random() * 6.28)
                           for y, x in rng.sample(sky, min(count, len(sky)))]
        return self._stars

    def frame(self, tod, n):
        pal = self.palettes[tod]
        cells = []
        below = self.h - self.horizon
        for y in range(self.h):
            if y >= self.horizon and self.horizon_is_water and "water" in pal:
                # below the horizon a "transparent" cell shows the water, not the sky
                sky = gradient(pal["water"], (y - self.horizon) / max(below - 1, 1))
            else:
                sky = gradient(pal["sky"], y / max(self.horizon - 1, 1))
            row = []
            for x in range(self.w):
                key, ch = self.mask[y][x], self.art[y][x]
                fg, bg = (255, 255, 255), sky
                spec = pal.get(key) if key != " " else None
                if spec:
                    fg = hex_rgb(spec["fg"])
                    bg = hex_rgb(spec["bg"]) if "bg" in spec else sky
                row.append([ch, fg, bg])
            cells.append(row)
        for e in self.meta.get("effects", []):
            if active(e, tod):
                EFFECTS[e["type"]](self, e, cells, pal, tod, n)
        return cells

    def check(self):
        keys = {k for r in self.mask for k in r} - {" "}
        for e in self.meta.get("effects", []):
            if e["type"] == "sprite":
                masks = e.get("mask_frames", [e["mask"]] if "mask" in e else [])
                keys |= {k for m in masks for r in m for k in r} - {".", " "}
        water = {e.get("key", "w") for e in self.meta.get("effects", []) if e["type"] == "waves"}
        keys -= water  # painted by the waves effect from `water` / `wave`
        problems = []
        for t in TIMES:
            missing = sorted(k for k in keys if k not in self.palettes[t])
            if missing:
                problems.append(f"{t}: mask keys without a colour: {''.join(missing)}")
            required = ["sky"] + (["water", "wave"] if water else [])
            for req in required:
                if req not in self.palettes[t]:
                    problems.append(f"{t}: palette has no {req!r}")
            for n in (0, 7, 55, 311):
                self.frame(t, n)
        return problems


CELL_W, CELL_H = 6, 12


def ink(ch, px, py):
    """Crude glyph rasteriser for --png: is sub-pixel (px, py) of a cell inked for `ch`?"""
    cx, cy = px / CELL_W, py / CELL_H
    if ch == " ":
        return False
    if ch == "|":
        return 0.35 < cx < 0.65
    if ch == "_":
        return cy > 0.8
    if ch in "-=~":
        return 0.42 < cy < 0.58 or (ch == "=" and 0.62 < cy < 0.72)
    if ch in ".,":
        return cy > 0.7 and 0.3 < cx < 0.7
    if ch in "'`":
        return cy < 0.35 and 0.35 < cx < 0.65
    if ch == "/":
        return abs((1 - cy) - cx) < 0.18
    if ch == "\\":
        return abs(cy - cx) < 0.18
    if ch in "[(":
        return cx < 0.35
    if ch in "])":
        return cx > 0.65
    if ch == "^":
        return cy < 0.5 and 0.4 - cy * 0.8 - 0.1 < abs(cx - 0.5) < 0.5 - cy * 0.8 + 0.12
    return 0.2 < cx < 0.8 and 0.3 < cy < 0.85


def write_png(cells, path):
    import struct
    import zlib
    h, w = len(cells) * CELL_H, len(cells[0]) * CELL_W
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        for x in range(w):
            ch, fg, bg = cells[y // CELL_H][x // CELL_W]
            raw += bytes(fg if ink(ch, x % CELL_W, y % CELL_H) else bg)

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
    Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                          + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))


def render(cells, top, left):
    out = []
    for i, row in enumerate(cells):
        out.append(f"\x1b[{top + i};{left}H")
        last = None
        for ch, fg, bg in row:
            if (fg, bg) != last:
                out.append(f"\x1b[38;2;{fg[0]};{fg[1]};{fg[2]}m\x1b[48;2;{bg[0]};{bg[1]};{bg[2]}m")
                last = (fg, bg)
            out.append(ch)
        out.append("\x1b[0m")
    return "".join(out)


def run(scene, tod, cycle):
    import termios
    import tty
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    sys.stdout.write("\x1b[?1049h\x1b[?25l\x1b[2J")
    try:
        tty.setcbreak(fd)
        n, paused, switched = 0, False, time.monotonic()
        while True:
            cols, rows = shutil.get_terminal_size()
            if cols < scene.w or rows < scene.h + 2:
                sys.stdout.write(f"\x1b[2J\x1b[H terminal too small: need {scene.w}x{scene.h + 2}, have {cols}x{rows}")
            else:
                top = (rows - scene.h - 2) // 2 + 1
                left = (cols - scene.w) // 2 + 1
                sys.stdout.write(render(scene.frame(tod, n), top, left))
                label = f"{scene.name} · {LABELS[tod]} · frame {n}   q quit · t next palette · space pause"
                sys.stdout.write(f"\x1b[{top + scene.h + 1};{left}H\x1b[2m{label.ljust(scene.w)}\x1b[0m")
            sys.stdout.flush()
            if select.select([sys.stdin], [], [], 1 / FPS)[0]:
                key = os.read(fd, 1)
                if key in (b"q", b"\x1b"):
                    break
                if key == b" ":
                    paused = not paused
                if key == b"t":
                    tod = TIMES[(TIMES.index(tod) + 1) % 4]
                    sys.stdout.write("\x1b[2J")
            if cycle and time.monotonic() - switched > 4:
                tod, switched = TIMES[(TIMES.index(tod) + 1) % 4], time.monotonic()
            if not paused:
                n += 1
    except KeyboardInterrupt:
        pass
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        sys.stdout.write("\x1b[0m\x1b[?25h\x1b[?1049l")
        sys.stdout.flush()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scene", nargs="?", default="galata")
    ap.add_argument("--time", choices=TIMES)
    ap.add_argument("--cycle", action="store_true", help="switch palette every 4 seconds")
    ap.add_argument("--plain", action="store_true", help="print the raw art once and exit")
    ap.add_argument("--check", action="store_true", help="validate the scene and render frames headless")
    ap.add_argument("--png", metavar="PATH", help="write all four palettes, stacked, to a PNG (crude glyphs)")
    ap.add_argument("--frame", type=int, default=9, help="frame number for --png")
    args = ap.parse_args()

    scene = Scene(args.scene)
    if args.plain:
        print("\n".join(r.rstrip() for r in scene.art))
        return
    if args.png:
        gap = [[" ", (0, 0, 0), (0, 0, 0)]] * scene.w
        stacked = []
        for t in TIMES:
            stacked += scene.frame(t, args.frame) + [gap]
        write_png(stacked, args.png)
        print(args.png)
        return
    if args.check:
        problems = scene.check()
        print(f"{scene.name}: {scene.w}x{scene.h}, {len(scene.meta.get('effects', []))} effects, "
              + ("OK" if not problems else "PROBLEMS"))
        for p in problems:
            print("  -", p)
        sys.exit(1 if problems else 0)
    run(scene, args.time or time_of_day(), args.cycle)


if __name__ == "__main__":
    main()
