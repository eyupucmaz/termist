"""One-off authoring aid: composes the Yerebatan Sarnici (Basilica Cistern) large scene.

Usage: python3 tools/scenes/build_yerebatan.py assets/scenes/yerebatan

The column forest is laid out on a real grid (bays of BAY metres) and projected with a
one-point perspective, so the recession is exact: rank k of columns stands at depth
Z = Z0 + k * BAY and is drawn at scale s = F / Z, far to near (painter's algorithm), with its
brick arcade, marble shafts, base lamps and a broken reflection in the water. The lamps light
the aisle; the far side bays sink into the dark. The Medusa head is a hand-drawn block under
the nearest left column.
"""
import math
import sys
from pathlib import Path

W, H = 96, 26

# --- camera / grid -------------------------------------------------------------------
VX, VY = 58, 12          # vanishing point (col, row); VY is the eye-level / horizon row
F = 11.0                 # focal length, rows per metre at Z = 1
ASPECT = 3.0             # columns per row for the same length (cells are ~2:1, widened for the arches)
CX = -0.4                # camera sits this far left of the aisle's centre line, metres
HC = 4.5                 # eye height above the water, metres
BAY = 4.9                # column spacing, metres (both directions)
D = 0.9                  # shaft diameter, metres
CAP_TOP = 8.0            # capital top (arch springing) above the water, metres
Z0 = 5.5                 # depth of the nearest rank
RANKS = 5
LINES = range(-6, 2)     # column lines left..right; the aisle runs between j=-1 and j=0
LAMP_LINES = {-3, -2, -1, 0}
REACH = [9, 2, 1, 1, 0]  # per rank: side lines shown beyond the aisle pair
WATER_MOVING_FROM = 20   # rows above this are still water ('v'), below it waves ('w')


def col_of(x, z):
    return VX + ASPECT * F * (x - CX) / z


def row_of(yw, z):       # yw = height above the water
    return VY + F * (HC - yw) / z


def line_x(j):           # lateral position of column line j
    return (j + 0.5) * BAY


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

    def putm(self, row, col, text, keys):
        """text with a same-length per-char key string; '.' key = transparent."""
        assert len(text) == len(keys), (text, keys)
        for i, (ch, k) in enumerate(zip(text, keys)):
            if k != ".":
                self.put(row, col + i, ch, k)

    def dump(self):
        return ("\n".join("".join(r).rstrip() for r in self.art) + "\n",
                "\n".join("".join(r).rstrip() for r in self.mask) + "\n")


def hashf(*a):
    """Deterministic pseudo-random 0..1 from integers."""
    h = 2166136261
    for v in a:
        h = ((h ^ (v & 0xFFFF)) * 16777619) & 0xFFFFFFFF
    return (h % 10007) / 10007


def curve(points, steep="|"):
    """Rasterise a polyline of float (col, row) points into (row, col, ch) cells. Every crossing
    of a cell centre line (column or row) yields a cell, so there are no gaps; then cells whose
    neighbours along the path already touch are dropped, so diagonals stay one glyph thick.
    The glyph follows the local slope."""
    path = []                                   # (row, col, ch) in path order, unique cells
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        dx, dy = x1 - x0, y1 - y0
        if dx == 0 and dy == 0:
            continue
        hits = []
        if dx:
            lo, hi = sorted((x0, x1))
            hits += [(c, y0 + (c - x0) * dy / dx) for c in range(math.ceil(lo - 1e-9), math.floor(hi + 1e-9) + 1)]
        if dy:
            lo, hi = sorted((y0, y1))
            hits += [(x0 + (r - y0) * dx / dy, r) for r in range(math.ceil(lo - 1e-9), math.floor(hi + 1e-9) + 1)]
        hits.sort(key=lambda h: (h[0] - x0) * dx + (h[1] - y0) * dy)   # along the segment
        a = abs(dy / dx) if dx else math.inf
        for x, y in hits:
            r, c = math.floor(y + 0.5), math.floor(x + 0.5)
            if path and path[-1][:2] == (r, c):
                continue
            yc = y0 + (c - x0) * dy / dx if dx else y   # height of the curve at the cell's centre
            f = min(max(yc - (r - 0.5), 0), 1)          # 0 = top of the cell, 1 = bottom
            if a < 0.3:
                ch = "_" if f > 0.62 else ("-" if f > 0.2 else "'")
            elif a < 0.9:
                ch = "," if f > 0.5 else "`"          # gentle slope: '.' / "'", settled below
            elif a < 2.4 or steep == "/\\":
                ch = "\\" if (dy > 0) == (dx > 0) else "/"
            elif steep == "()":
                ch = "(" if dx * dy < 0 else ")"
            else:
                ch = steep
            path.append((r, c, ch))
    thin = []
    for i, cell in enumerate(path):
        if thin and i + 1 < len(path):
            (pr, pc, _), (nr, nc, _) = thin[-1], path[i + 1]
            if max(abs(pr - nr), abs(pc - nc)) <= 1:
                continue
        thin.append(cell)
    # a gentle slope crossing several cells of one row reads best as ".-'" (rising) or "'-." (falling)
    out, i = [], 0
    while i < len(thin):
        j = i
        while j + 1 < len(thin) and thin[j + 1][0] == thin[i][0] and thin[j + 1][2] in ",`" and thin[i][2] in ",`":
            j += 1
        run = thin[i:j + 1]
        if len(run) >= 2:
            rising = j + 1 < len(thin) and thin[j + 1][0] < run[0][0] or i > 0 and thin[i - 1][0] > run[0][0]
            glyphs = (".'" if rising else "'.") if len(run) == 2 else \
                (("." + "-" * (len(run) - 2) + "'") if rising else ("'" + "-" * (len(run) - 2) + "."))
            run = [(r, c, g) for (r, c, _), g in zip(run, glyphs)]
        elif run[0][2] in ",`" and 0 < i and j + 1 < len(thin) and \
                thin[i - 1][0] != run[0][0] and thin[j + 1][0] != run[0][0]:
            # a lone step between rows is a diagonal, not a tick
            r, c, _ = run[0]
            run = [(r, c, "/" if thin[j + 1][0] < r else "\\")]
        elif run[0][2] in ",`" and (i > 0 and thin[i - 1][0] == run[0][0] or
                                    j + 1 < len(thin) and thin[j + 1][0] == run[0][0]):
            # beside a flat stretch, a lone gentle cell is the lower end of the bend
            r, c, _ = run[0]
            run = [(r, c, ".")]
        out += [(r, c, {",": ".", "`": "'"}.get(g, g)) for r, c, g in run]
        i = j + 1
    return out


def half_ellipse(cx, cy, rx, ry, n=80):
    return [(cx - rx * math.cos(math.pi * i / n), cy - ry * math.sin(math.pi * i / n)) for i in range(n + 1)]


def rank_keys(k):
    """Atmospheric depth: nearer ranks are brighter and filled; far ones are dim lines."""
    if k == 0:
        return dict(out="C", fill="c", cap="A", brick="B", bfill="b", refl="x", lamp="L")
    if k == 1:
        return dict(out="I", fill="i", cap="I", brick="B", bfill="b", refl="x", lamp="L")
    if k <= 3:
        return dict(out="Y", fill="Y", cap="Y", brick="N", bfill=None, refl="z", lamp="O")
    return dict(out="Z", fill="Z", cap="Z", brick="N", bfill=None, refl=None, lamp="O")


def visible_lines(z, k, margin=32):
    """Lines a rank shows: all of them near by, only the lit aisle far away."""
    reach = REACH[min(k, len(REACH) - 1)]
    return [j for j in LINES if -1 - reach <= j <= reach and -margin < col_of(line_x(j), z) < W + margin]


cv = Canvas()

# --- water: everything below eye level --------------------------------------------------
for row in range(VY + 1, H):
    for x in range(W):
        if row < WATER_MOVING_FROM:
            cv.put(row, x, " ", "v")
        else:
            cv.put(row, x, "~" if hashf(row, x, 7) < 0.12 else " ", "w")


def draw_rank(k):
    z = Z0 + BAY * k
    s = F / z
    keys = rank_keys(k)
    spring, base = row_of(CAP_TOP, z), row_of(0, z)
    w = max(1, round(ASPECT * s * D))
    capw = w + 2 if w >= 3 else w
    # semicircular arches springing from the capitals' outer corners, just above the impost
    rx = ASPECT * s * BAY / 2 - capw / 2 + 0.5
    ry = rx / ASPECT
    cy = round(spring) - 1
    band = s * 0.6                             # brick arch thickness in rows
    lines = visible_lines(z, k)
    bays = range(lines[0] + 1, lines[-1] + 1)  # arch j spans lines j-1 .. j
    xl = max(0, math.floor(col_of(line_x(lines[0]), z)))
    xr = min(W, math.ceil(col_of(line_x(lines[-1]), z)) + 1)

    # 1. the arcade: the vault above it hides everything farther; brick band on the intrados
    for x in range(xl, xr):
        intrados = spring
        for j in bays:
            u = (x - col_of(j * BAY, z)) / rx
            if abs(u) <= 1:
                intrados = cy - ry * math.sqrt(1 - u * u)
        for r in range(0, VY + 1):
            if r < intrados - band - 0.5:
                cv.put(r, x, " ", " ")
            elif r < intrados - 0.5 and keys["bfill"]:
                cv.put(r, x, " ", keys["bfill"])
    for j in bays:
        for r, c, ch in curve(half_ellipse(col_of(j * BAY, z), cy, rx, ry), "()" if k == 0 else "/\\"):
            cv.put(r, c, ch, keys["brick"])

    # 2. columns: capital, shaft (warm near the lamp), lamp at the base
    for j in lines:
        left = round(col_of(line_x(j), z) - w / 2)
        r_top, r_base = round(spring), round(base)
        lamp = j in LAMP_LINES and (k, j) != (0, -2)   # (0, -2) stands on the Medusa head
        if w >= 3:
            cv.put(r_top, left - 1, "[" + "=" * w + "]", keys["cap"])
            cv.put(r_top + 1, left, "\\" + "_" * (w - 2) + "/", keys["cap"])
            r_shaft = r_top + 2
            for r in range(r_shaft, r_base):
                up = r_base - r                        # rows above the lamp: glow fades upward
                glow = lamp and up <= round(s * 2.2)
                fill = ("u" if up <= round(s * 1.0) else "e") if glow else keys["fill"]
                cv.put(r, left, "|", "U" if glow else keys["out"])
                cv.put(r, left + 1, " " * (w - 2), fill)
                cv.put(r, left + w - 1, "|", "U" if glow else keys["out"])
            cv.put(r_base, left, "|" + "_" * (w - 2) + "|", "U" if lamp else keys["out"])
            lamp_c = left + w // 2
            if lamp:
                cv.put(r_base, lamp_c, "*", keys["lamp"])
        else:
            cv.put(r_top, left, "T" if w == 1 else "[]", keys["cap"])
            r_shaft = r_top + 1
            for r in range(r_shaft, r_base):
                cv.put(r, left, "|" * w, keys["out"])
            lamp_c = left
            if lamp:
                cv.put(r_base, lamp_c, "o" if k <= 2 else ".", keys["lamp"])

        # 3. broken reflection, mirrored about the waterline, wobbling sideways
        if keys["refl"]:
            for r in range(r_shaft, r_base):
                rr = 2 * r_base - r
                if not (r_base < rr < H) or hashf(k, j, rr) < 0.28:   # broken: rows missing
                    continue
                if w >= 3:
                    cv.put(rr, left + (0, 1, 0, -1)[rr % 4], ":" + " " * (w - 2) + ":", keys["refl"])
                else:
                    cv.put(rr, left, ":" if rr % 2 else "'", keys["refl"])
            if lamp:                                   # the lamp's light: a warm streak down the water
                for d, ch in enumerate("!:." if w >= 3 else ":."):
                    cv.put(r_base + 1 + d, lamp_c, ch, "l")


for k in range(RANKS - 1, -1, -1):
    draw_rank(k)

# --- the Medusa head: an upside-down carved block under the near-left column --------------
# As in the cistern's north-west corner: a big marble block, the face carved in it upside down,
# a ring of coiling snakes for hair, the column shaft rising straight out of its top, a lamp at
# its foot lighting it from below. Upside down the face reads top to bottom: chin, full lips,
# nostrils, straight nose, eyes, heavy lids, a strong brow line, then the forehead.
# Keys: k block, j snakes, q lit face, Q carved features, E eyes, L the lamp at its foot.
FACE = [                                         # upside down; per-char keys follow below
    "     _.-'''-._     ",
    "   .'  _____  '.   ",
    "  /   (_____)   \\  ",
    " |      (^)      | ",
    " |       |       | ",
    " |  (_@_) (_@_)  | ",
    " |   ===   ===   | ",
    "  \\ ~~~~   ~~~~ /  ",
    "   '-._______.-'   ",
]
FACE_KEYS = [                                    # Q carved line, q lit stone, n mouth, E eye sockets
    "     QQQQQQQQQ     ",
    "   QQqqQQQQQqqQQ   ",
    "  QqqqnnnnnnnqqqQ  ",
    " QqqqqqqQQQqqqqqqQ ",
    " QqqqqqqqQqqqqqqqQ ",
    " QqqEEEEEqEEEEEqqQ ",
    " QqqqQQQqqqQQQqqqQ ",
    "  QqQQQQqqqQQQQqQ  ",
    "   QQQQQQQQQQQQQ   ",
]
MED_W = 31
HALO = 5                                         # snake band around the face, chars per side
SNAKES = "S~(@)~"


def snake_run(n, row, mirror=False):
    s = (SNAKES * 8)[(row * 2) % len(SNAKES):][:n]
    if mirror:
        s = s[::-1].translate(str.maketrans("()", ")("))
    return s


def medusa_rows():
    shaft = max(1, round(ASPECT * F / Z0 * D))
    top = "_" * ((MED_W - shaft) // 2)
    rows = [(top + "|" + " " * (shaft - 2) + "|" + top, "k" * len(top) + "." * shaft + "k" * len(top))]
    fw = len(FACE[0])
    side = (MED_W - fw) // 2
    for i, (f, fk) in enumerate(zip(FACE, FACE_KEYS)):
        assert len(f) == len(fk) == fw, (f, fk)
        lead = len(f) - len(f.lstrip(" "))
        n = len(f.strip(" "))
        core, ckeys = f[lead:lead + n], fk[lead:lead + n]
        pad = side + lead                        # cells left of the face
        halo = max(0, min(pad - 1, HALO + (1 if 0 < i < len(FACE) - 1 else -1)))
        plain = pad - 1 - halo
        left = "|" + " " * plain + snake_run(halo, i)
        right = snake_run(halo, i, mirror=True) + " " * plain + "|"
        rows.append((left + core + right,
                     "k" * (1 + plain) + "j" * halo + ckeys + "j" * halo + "k" * (plain + 1)))
    half = (MED_W - 5) // 2                      # block foot at the waterline, its lamp in the middle
    rows.append(("|" + "_" * half + "_*_" + "_" * half + "|", "k" * (half + 1) + "kLk" + "k" * (half + 1)))
    return rows


MEDUSA = medusa_rows()
MED_COL = round(col_of(line_x(-2), Z0)) - MED_W // 2              # centred under line -2, rank 0
MED_ROW = round(row_of(0, Z0)) - len(MEDUSA) + 1                  # standing in the water
for i, (t, k) in enumerate(MEDUSA):
    assert len(t) == len(k) == MED_W, (t, k)
    cv.putm(MED_ROW + i, MED_COL, t, k)
# a subtle reflection: the face the right way up (eyes, lids, brow), broken by ripples
FLIP = {"/": "\\", "\\": "/", "'": ".", "_": "-", "^": "v"}
for i, (t, k) in enumerate(reversed(MEDUSA[1:-1])):
    rr = MED_ROW + len(MEDUSA) + i
    for dx, (ch, key) in enumerate(zip(t, k)):
        if ch == " " or key in "k" or hashf(rr, dx, 3) < (0.8 if key == "j" else 0.4):
            continue
        cv.put(rr, MED_COL + dx + (i % 2), FLIP.get(ch, ch), "x" if key != "j" else "z")

art, mask = cv.dump()
out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("assets/scenes/yerebatan")
out.mkdir(parents=True, exist_ok=True)
(out / "large.art.txt").write_text(art)
(out / "large.mask.txt").write_text(mask)
