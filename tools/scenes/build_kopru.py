"""One-off authoring aid: composes the Bosphorus Bridge (Bogazici Koprusu) scene, seen from Ortakoy.

The main cable is a true catenary solved in physical units (a terminal cell is about twice as tall
as it is wide), the hangers drop from it to the deck at a fixed pitch, and the backstays run from the
tower tops to anchorages on the hills. Writes large.art.txt / large.mask.txt into the given folder.

    python3 tools/scenes/build_kopru.py assets/scenes/kopru

Mask keys of this scene (shared ones: see assets/scenes/README.md):
    C main cable + backstays (LED colour cycle at night)   c hangers (same cycle)
    B deck                    P / O European / Asian tower  Q / q mosque marble outline / fill
    Y / y European hill       A / a Asian hill              Z / z far shore up the strait
    e trees                   N anchorage blocks        X gilded alems (crescent finials)
"""
import math
import sys
from pathlib import Path

W, H = 96, 26
ASPECT = 2.0            # a cell is ~2x as tall as it is wide


class Canvas:
    def __init__(self):
        self.art = [[" "] * W for _ in range(H)]
        self.mask = [[" "] * W for _ in range(H)]

    def put(self, row, col, text, key):
        for i, ch in enumerate(text):
            c = col + i
            if 0 <= row < H and 0 <= c < W:
                self.art[row][c] = ch
                self.mask[row][c] = key

    def put_solid(self, row, col, text, key):
        for i, ch in enumerate(text):
            if ch != " ":
                self.put(row, col + i, ch, key)

    def putm(self, row, col, text, keys):
        """text with a same-length per-char key string; '.' in keys = keep what's underneath."""
        assert len(text) == len(keys), (text, keys)
        for i, (ch, k) in enumerate(zip(text, keys)):
            if k != ".":
                self.put(row, col + i, ch, k)

    def key(self, row, col):
        return self.mask[row][col] if 0 <= row < H and 0 <= col < W else None

    def dump(self):
        return ("\n".join("".join(r).rstrip() for r in self.art) + "\n",
                "\n".join("".join(r).rstrip() for r in self.mask) + "\n")


# --------------------------------------------------------------------------- curve drawing
def curve_cells(f, x0, x1):
    """Cells (row, col, char) tracing y = f(x) (row units, 0 = top of row 0) for cols x0..x1.

    Gentle stretches use the sub-row glyphs ' - . _ (top, middle, baseline, bottom) so the line
    sags smoothly; steep stretches use / and \\, with a run of them where a column drops more than a
    row, so the line never breaks."""
    out = []
    rows = {x: int(math.floor(f(x + 0.5))) for x in range(x0 - 1, x1 + 2)}
    for x in range(x0, x1 + 1):
        yc = f(x + 0.5)
        slope = f(x + 1.0) - f(x)             # rows per column, + = going down to the right
        if abs(slope) < 0.7:
            row, frac = rows[x], yc - math.floor(yc)
            if frac < 0.30:
                ch = "'"
            elif frac < 0.62:
                ch = "-"
            else:
                ch = "_" if abs(slope) < 0.3 or frac > 0.85 else "."
            out.append((row, x, ch))
        else:
            ch = "\\" if slope > 0 else "/"
            if abs(slope) > 2.6:
                ch = "|"
            # cover this column's own row, plus any rows skipped before the next column
            nxt = rows[x + 1] if slope > 0 else rows[x - 1]
            for r in range(rows[x], max(rows[x], nxt - 1) + 1):
                out.append((r, x, ch))
    # a baseline '_' right next to a mid-height '-' reads as a step: soften it to '.'
    at = {(r, x): i for i, (r, x, _) in enumerate(out)}
    for i, (r, x, ch) in enumerate(out):
        if ch == "_" and any(out[at[(r, x + d)]][2] == "-" for d in (-1, 1) if (r, x + d) in at):
            out[i] = (r, x, ".")
    return out


def catenary(xa, xb, y_top, y_low):
    """y(x) of a cable hanging between (xa, y_top) and (xb, y_top) whose lowest point is y_low."""
    half = (xb - xa) / 2                  # physical units: 1 col = 1, 1 row = ASPECT
    sag = (y_low - y_top) * ASPECT
    lo, hi = 0.01, 1e6                    # solve a*(cosh(half/a) - 1) = sag for a
    for _ in range(200):
        a = (lo + hi) / 2
        if a * (math.cosh(half / a) - 1) > sag:
            lo = a
        else:
            hi = a
    xm = (xa + xb) / 2
    return lambda x: y_low - a * (math.cosh((x - xm) / a) - 1) / ASPECT


def line(xa, ya, xb, yb, sag=0.0):
    """A nearly straight backstay from (xa, ya) to (xb, yb), sagging `sag` rows in the middle."""
    def f(x):
        t = (x - xa) / (xb - xa)
        return ya + (yb - ya) * t + sag * 4 * t * (1 - t)
    return f


def smooth(t):
    t = min(max(t, 0.0), 1.0)
    return t * t * (3 - 2 * t)


cv = Canvas()

# --------------------------------------------------------------------------- layout
TOP = 1            # tower tops: the cable saddles
DECK = 13          # deck row
LOW = 11.75        # lowest point of the main cable (row units, just above the deck)
SHORE = 19         # last land row on the far side; open water starts at row 20
XL, XR = 41, 79    # tower centre columns (legs at +-2)


def hill_eu(x):    # European shore behind the mosque, falling to the water at the left tower
    return 12.5 + 5.6 * smooth((x - 26) / 18)


def hill_as(x):    # Asian shore (Beylerbeyi), rising to the right
    return 18.6 - 4.8 * smooth((x - 77) / 20) - 0.3 * math.sin(x / 2.1) * smooth((x - 84) / 6)


def hill_far(x):   # distant shore up the strait, seen through the bridge
    return 16.2 + 1.1 * math.sin(x / 3.4 + 1.0) + 0.3 * math.sin(x / 1.4)


def paint_hill(f, x0, x1, edge, fill, bottom=SHORE):
    for x in range(x0, x1 + 1):
        for rr in range(int(math.floor(f(x + 0.5))) + 1, bottom + 1):
            cv.put(rr, x, " ", fill)
    for r, x, ch in curve_cells(f, x0, x1):
        if r <= bottom:
            cv.put(r, x, ch, edge)


# --- far shore, then both hills (the Asian one sits in front of the far shore) ---------
paint_hill(hill_far, 40, 80, "Z", "z")
paint_hill(hill_eu, 0, XL + 3, "Y", "y")
paint_hill(hill_as, 64, W - 1, "A", "a")

# --- houses: yalis on the Asian waterfront, a few on both hills, trees ----------------
def tiny_house(col, base):
    cv.put(base - 1, col, "/_\\", "T")
    cv.putm(base, col, "|n|", "HWH")


def wide_house(col, base):
    cv.put(base - 1, col, "/___\\", "T")
    cv.putm(base, col, "|n n|", "HWhWH")


def tree(col, row):
    cv.put(row, col, "o", "e")


def yali(col, base):     # waterfront mansion, two storeys
    cv.put(base - 3, col, " _______ ", "T")
    cv.put(base - 2, col, "/_______\\", "T")
    cv.putm(base - 1, col, "|n n n n|", "HWhWhWhWH")
    cv.putm(base, col, "|n_n_n_n|", "HWHWHWHWH")


yali(62, 18)
wide_house(72, 18)
tiny_house(32, 15)
tiny_house(84, int(hill_as(85.5)) + 2)
wide_house(89, int(hill_as(91.5)) + 2)
for col, row in ((88, 18), (95, 17), (70, 19)):
    if cv.key(row, col) in ("a", "y"):
        tree(col, row)

# --- evening lights: a few dots on the far shore and the Asian hill (they twinkle at night) ---
for col in (47, 51, 55, 58, 64, 68, 73, 76):
    r = int(math.floor(hill_far(col + 0.5))) + 2
    if cv.key(r, col) == "z":
        cv.put(r, col, ".", "L")
for col in (80, 83, 87, 92, 95):
    r = int(math.floor(hill_as(col + 0.5))) + 2
    if cv.key(r, col) == "a":
        cv.put(r, col, ".", "l")

# --- deck: from the European hill to the Asian hill --------------------------------------
for x in range(W):
    ground = hill_eu(x + 0.5) if x < 60 else hill_as(x + 0.5)
    if ground > DECK + 0.3:
        cv.put(DECK, x, "=", "B")

# --- main cable (catenary) + hangers ----------------------------------------------------
main = catenary(XL + 3, XR - 2, TOP, LOW)          # from saddle edge to saddle edge
lowest = {}
for r, x, ch in curve_cells(main, XL + 3, XR - 3):
    cv.put(r, x, ch, "C")
    lowest[x] = max(lowest.get(x, -1), r)
for x, r in lowest.items():
    if (x - XL) % 2 == 1 and XL + 4 < x < XR - 4:
        for rr in range(r + 1, DECK):
            cv.put(rr, x, "|", "c")

# --- backstays down to the anchorages ---------------------------------------------------
ANCHOR_EU = 27
ANCHOR_AS = 94
back_eu = line(ANCHOR_EU, hill_eu(ANCHOR_EU), XL - 2, TOP, 0.2)
for r, x, ch in curve_cells(back_eu, ANCHOR_EU, XL - 3):
    cv.put(r, x, ch, "C")
back_as = line(XR + 3, TOP, ANCHOR_AS + 1, hill_as(ANCHOR_AS) + 0.3, 0.2)
for r, x, ch in curve_cells(back_as, XR + 3, ANCHOR_AS):
    cv.put(r, x, ch, "C")
cv.put(int(hill_as(ANCHOR_AS)) + 1, ANCHOR_AS - 1, "[]", "N")

# --- towers: portal frames of two legs and three cross-beams -----------------------------
for xc, key in ((XL, "P"), (XR, "O")):
    ground = hill_eu(xc) if key == "P" else hill_as(xc)
    foot = SHORE if key == "O" else 17          # the European legs stand on the quay
    cv.put(TOP - 1, xc - 2, "_____", key)
    for r in range(TOP, foot + 1):
        cv.put(r, xc - 2, "|", key)
        cv.put(r, xc + 2, "|", key)
        if r in (TOP, TOP + 6, DECK + 1):
            cv.put(r, xc - 1, "===", key)
        elif r == DECK:
            cv.put(r, xc - 1, "===", "B")
        else:
            cv.put(r, xc - 1, "   ", " " if r < ground else ("a" if key == "O" else "y"))

# --- sun / moon, framed by the sag of the cable -------------------------------------------
cv.put(3, 57, " .--. ", "M")
cv.put(4, 57, "(    )", "M")
cv.put(5, 57, " '--' ", "M")

# --- Ortakoy Mosque, foreground left, on its quay ------------------------------------------
c = 17                         # axis of the mosque


def minaret(m):
    """Pencil minaret standing on a corner of the prayer hall, one serefe (balcony), gilded alem."""
    cv.put(1, m, "C", "X")                         # alem: crescent on a short rod
    cv.put(2, m, "|", "X")
    cv.put(3, m, "^", "R")
    cv.putm(4, m - 1, "/ \\", "RrR")
    for r in range(5, 10):
        cv.putm(r, m - 1, "| |", "QqQ")
    cv.put(6, m - 2, "[===]", "D")
    cv.putm(7, m - 1, "\\ /", "QqQ")


minaret(c - 12)
minaret(c + 12)

# dome on its windowed drum
def wall(row, col, text):
    """Interior of a filled wall: 'n' is a window (lit at night), everything else sits on the fill."""
    cv.putm(row, col, text, "".join("W" if ch == "n" else "q" for ch in text))


cv.put(2, c, "C", "X")                             # the dome's alem, its rod rising from the apex
cv.putm(3, c - 3, "_.-|-._", "RRRXRRR")
cv.putm(4, c - 5, ".'" + " " * 7 + "'.", "RR" + "r" * 7 + "RR")
cv.putm(5, c - 6, "/" + " " * 11 + "\\", "R" + "r" * 11 + "R")
cv.put(6, c - 7, "[=============]", "D")
for r, inner in ((7, " n n n n n "), (8, "_" * 11)):
    cv.put(r, c - 6, "|", "Q")
    wall(r, c - 5, inner)
    cv.put(r, c + 6, "|", "Q")
cv.putm(9, c - 10, "_.-'" + " " * 13 + "'-._", "QQQQ" + "q" * 13 + "QQQQ")

# prayer hall: cornice, a big central arch between two arched bays, lower windows, the door
cv.put(10, c - 13, "[" + "=" * 25 + "]", "D")
hall = [
    "  ___     ___     ___  ",
    " /   \\  .'   '.  /   \\ ",
    " |n n|  | n n |  |n n| ",
    " |___|  |     |  |___| ",
    "        | .-. |        ",
    "  n n   | | | |   n n  ",
    "_" * 23,
]
for i, inner in enumerate(hall):
    assert len(inner) == 23, (i, len(inner))
    cv.put(11 + i, c - 12, "|", "Q")
    wall(11 + i, c - 11, inner)
    cv.put(11 + i, c + 12, "|", "Q")

# --- quay: the mosque's terrace runs out to the European tower ----------------------------
QUAY = XL + 5
cv.put(18, 0, "_" * QUAY, "s")
cv.put(18, c - 13, "[" + "=" * 25 + "]", "s")
for r in (19, 20):
    brick = ("__|___" if r == 19 else "___|__") * 20
    cv.put(r, 0, brick[:QUAY - 2] + "_", "s")
    cv.put(r, QUAY - 1, "|", "s")

# --- water ----------------------------------------------------------------------------------
waves = ["~   ~~    ~  ", "  ~    ~~~   ", "~~   ~     ~ ", "   ~~   ~   ~", " ~     ~~  ~ ", "~  ~~      ~ "]
for i, row in enumerate(range(20, H)):
    pat = (waves[i % len(waves)] * 10)[:W]
    for x in range(W):
        if cv.key(row, x) in (" ", "z", "Z"):
            cv.put(row, x, pat[x], "w")

art, mask = cv.dump()
out = Path(sys.argv[1] if len(sys.argv) > 1 else "assets/scenes/kopru")
out.mkdir(parents=True, exist_ok=True)
(out / "large.art.txt").write_text(art)
(out / "large.mask.txt").write_text(mask)
print(art)
