"""One-off authoring aid: composes the Vapur ve marti scene.

Writes large.art.txt / large.mask.txt (the static canvas: sky, far shores, water) and
scene.toml (effects, including the ferry and its gull escort as sprites).

    python3 tools/scenes/build_vapur.py assets/scenes/vapur
"""
import sys
from pathlib import Path

W, H = 96, 26
SHORE_BASE = 12     # last land row of the far shores
WATER_TOP = 13      # first full-width water row (row 12 is water only between the shores)


class Canvas:
    def __init__(self, w=W, h=H):
        self.w, self.h = w, h
        self.art = [[" "] * w for _ in range(h)]
        self.mask = [[" "] * w for _ in range(h)]

    def put(self, row, col, text, key):
        for i, ch in enumerate(text):
            c = col + i
            if 0 <= row < self.h and 0 <= c < self.w:
                self.art[row][c] = ch
                self.mask[row][c] = key

    def putm(self, row, col, text, keys):
        """text with a same-length per-char key string; '.' in keys leaves the cell alone."""
        assert len(text) == len(keys), (text, keys)
        for i, (ch, k) in enumerate(zip(text, keys)):
            if k != ".":
                self.put(row, col + i, ch, k)

    def rows(self, transparent=None):
        art = ["".join(r) for r in self.art]
        mask = ["".join(r) for r in self.mask]
        if transparent:  # sprite: untouched cells become '.'
            mask = ["".join(transparent if m == " " else m for m in r) for r in mask]
        return art, mask

    def dump(self):
        return ("\n".join("".join(r).rstrip() for r in self.art) + "\n",
                "\n".join("".join(r).rstrip() for r in self.mask) + "\n")


cv = Canvas()

# --- water ------------------------------------------------------------------
# sparse ripples near the horizon, denser towards the viewer
WAVES = {
    12: "                  ~               ",
    13: "      ~                    ~          ",
    14: "  ~               ~~            ",
    15: "         ~~              ~        ~   ",
    16: "~             ~       ~~       ",
    17: "     ~~          ~          ~    ",
    18: "  ~       ~~~        ~     ",
    19: "~    ~         ~~      ~   ",
    20: "   ~~     ~      ~   ",
    21: "~     ~~    ~   ~~  ",
    22: "  ~   ~~    ~  ",
    23: " ~~     ~  ~   ",
    24: "~   ~~    ~  ",
    25: "  ~    ~~~   ",
}
for row, pat in WAVES.items():
    cv.put(row, 0, (pat * 12)[:W], "w")

# --- far shores ----------------------------------------------------------------
# Distant, hazy silhouettes: outline `A` sits on the sky, fill `a` is solid, `L` are
# the specks of houses that light up at night, `Y` the faint Princes' Islands.


def silhouette(rows, col):
    """rows: list of (row, text). Non-space chars are outline `A`; every cell below the
    topmost glyph of a column (down to SHORE_BASE) is filled `a`. ':' marks a lit window
    `L` inside the fill, '!' a lamp `Z`."""
    top = {}
    for row, text in rows:
        for i, ch in enumerate(text):
            if ch != " ":
                c = col + i
                top[c] = min(top.get(c, 99), row)
    for c, t in top.items():
        for r in range(t, SHORE_BASE + 1):
            cv.put(r, c, " ", "a")
    for row, text in rows:
        for i, ch in enumerate(text):
            c = col + i
            if ch == " ":
                continue
            if ch == ":":
                cv.put(row, c, ".", "L")
            elif ch == "!":
                cv.put(row, c, "*", "Z")
            elif ch == "#":
                cv.put(row, c, " ", "a")
            else:
                key = "A" if row == top[c] or ch in "|^" else "a"
                cv.put(row, c, ch, key)


# European side: the historic peninsula, Ayasofya and Sultanahmet on the ridge
silhouette([
    (6,  "            ^         ^           ^  ^     ^  ^"),
    (7,  "            |   _._   |           |  |  .  |  |"),
    (8,  "            |  /###\\  |           |  | /#\\ |  |"),
    (9,  "      _    _|_/#####\\_|_    __    | _|/###\\|_ |"),
    (10, "   __/ \\__/  :        :\\__/  \\___|/ :     : \\|__"),
    (11, "__/   :      :    :       :    :    :    :    \\_"),
    (12, "   :     :      :    :  :    :    :      :     \\_"),
], 0)

# Asian side: the Uskudar hills with the Camlica mosque on top
silhouette([
    (6,  "                  ^   _._   ^"),
    (7,  "                  |  /###\\  |"),
    (8,  "               ___|_/#####\\_|___"),
    (9,  "          __..-'  :    :     :  '-.._"),
    (10, "     _.-''    :      :    :   :     ''"),
    (11, "  .-'   :   :     :    :     :    :   "),
    (12, "-'   :     :    :    :    :    :     :"),
], 61)

# Kiz Kulesi, tiny on its rock off the Asian shore
silhouette([
    (8,  "  !  "),
    (9,  "  ^  "),
    (10, " |#| "),
    (11, "_|#|_"),
    (12, "(___)"),
], 54)

# carve the open sea between the shores on row 12
for c in range(W):
    if cv.mask[SHORE_BASE][c] == " ":
        cv.put(SHORE_BASE, c, " ", "w")

# --- sun / moon ---------------------------------------------------------------
cv.put(1, 80, " .--. ", "M")
cv.put(2, 80, "(    )", "M")
cv.put(3, 80, " '--' ", "M")

# --- the ferry (sprite) -----------------------------------------------------------
# Sehir Hatlari vapur, bow to the left (it sails right to left, the gulls' way).
FW, FH = 63, 10
f = Canvas(FW, FH)
B0, S1 = 3, 53           # bow and stern columns at the deck line (row 7)
M0, M1 = 6, 45           # main-deck cabin walls
U0, U1 = 11, 42          # upper-deck cabin walls
CH = 31                  # chimney, 4 wide

# hull: dark, a thin stripe along the deck line, portholes, raked bow and stern
f.put(7, B0, "\\", "x")
f.put(7, B0 + 1, "_" * (S1 - B0 - 1), "B")
f.put(7, S1, "/", "x")
f.put(8, B0 + 1, "\\", "x")
f.put(8, B0 + 2, " " * (S1 - B0 - 3), "k")
for c in range(B0 + 5, S1 - 3, 4):
    f.put(8, c, "o", "P")
f.put(8, S1 - 1, "/", "x")
f.put(9, B0 + 2, "\\", "x")
f.put(9, B0 + 3, "_" * (S1 - B0 - 5), "k")
f.put(9, S1 - 2, "/", "x")

# main deck: long white cabin, big windows two rows tall
f.put(4, M0 - 3, "_" * (S1 - M0 + 3), "V")          # roof edge, runs over the aft deck
for r in (5, 6):
    f.put(r, M0, "|", "v")
    f.put(r, M1, "|", "v")
    f.put(r, M0 + 1, " " * (M1 - M0 - 1), "v")
for c in range(M0 + 2, M1 - 5, 3):
    f.put(5, c, "  ", "O")
    f.put(6, c, "__", "O")
f.put(5, M1 - 3, "o", "U")                           # lifebuoy
f.put(6, M1 - 3, "_", "v")
f.put(5, M1 + 1, " | | | ", "V")                     # open aft deck
f.put(6, M1 + 1, "_|_|_|_", "V")
# foredeck bulwark
f.put(6, B0, "|__", "V")

# upper deck, sitting on the main-deck roof
f.put(3, U0, "|", "v")
f.put(3, U1, "|", "v")
f.put(3, U0 + 1, " " * (U1 - U0 - 1), "v")
for c in range(U0 + 2, U1 - 1, 3):
    f.put(3, c, "  ", "O")
f.put(4, U0, "|" + "_" * (U1 - U0 - 1) + "|", "v")   # its floor, the main-deck ceiling
# sun deck: railing on the upper-deck roof
f.put(2, U0 - 1, "_" + "|_" * ((U1 - U0 + 2) // 2), "J")
# wheelhouse forward, mast on it
f.put(1, U0, "_______", "J")
f.put(2, U0, "|", "v")
f.put(2, U0 + 1, "     ", "O")
f.put(2, U0 + 6, "|", "v")
f.put(0, U0 + 3, "+", "I")
f.put(1, U0 + 3, "|", "J")
# chimney: black cap over a yellow band
f.put(0, CH, "    ", "f")
f.put(1, CH, "    ", "c")
f.put(2, CH, "____", "c")
# Turkish flag on the stern staff, streaming aft
f.put(1, S1 - 2, "|", "J")
f.put(1, S1 - 1, "(*", "Q")
f.put(2, S1 - 2, "|", "J")
f.put(3, S1 - 2, "|", "J")
# bow wave and wake
f.put(9, 2, "~=-", "E")
f.put(8, S1, " ~-=_", "E")
f.put(9, S1 - 1, "~=-~-_ - ~", "E")
FERRY_ART, FERRY_MASK = f.rows(".")
FERRY_ROW = 10

# gulls escorting the ferry: same width + speed, so they hang over the stern and wake
g = Canvas(FW, 11)
for r, c, s in [(0, 50, "\\v/"), (2, 57, "-v-"), (3, 44, "v"), (1, 38, "-v-"), (10, 58, "\\v/"), (4, 61, "v")]:
    g.put(r, c, s, "N")
GULL_ART, GULL_MASK = g.rows(".")
GULL_ROW = FERRY_ROW - 4


def toml_lines(lines):
    for ln in lines:
        assert "'" not in ln, ln
    return "[\n" + "".join(f"  '{ln}',\n" for ln in lines) + "]"


SCENE = f"""# Vapur ve marti: a Sehir Hatlari ferry crossing the Bosphorus, gulls in its wake.
# Generated by tools/scenes/build_vapur.py (the sprites are composed there).
title = "Vapur ve martı"
horizon_key = "w"          # sky gradient ends where the open sea begins

[[effects]]
type = "waves"             # Bogaz
key = "w"
every = 2

[[effects]]
type = "stars"
count = 30
times = ["gece"]

[[effects]]
type = "twinkle"           # house lights on the far shores
key = "L"
threshold = 0.8
off = "#6c3d5c"
times = ["aksam"]

[[effects]]
type = "twinkle"
key = "L"
threshold = 0.8
off = "#262d50"
times = ["gece"]

[[effects]]
type = "blink"             # Kiz Kulesi's lamp
key = "Z"
colors = ["#fff1c2", "#b98a3a"]
period = 14
times = ["aksam", "gece"]

[[effects]]
type = "gulls"             # drawn before the ferry, so it sails in front of them
rows = [3, 7, 5, 9]
color = "#5f6977"          # grey backs read against the pale morning haze
times = ["sabah"]

[[effects]]
type = "gulls"
rows = [3, 7, 5, 9]
times = ["gunduz"]

[[effects]]
type = "gulls"
rows = [3, 7, 5, 9]
color = "#43283d"          # silhouettes against the sunset
times = ["aksam"]

[[effects]]
type = "sprite"            # the escort: same width and speed as the ferry, so it keeps station
art = {toml_lines(GULL_ART)}
mask = {toml_lines(GULL_MASK)}
row = {GULL_ROW}
speed = 0.55
direction = -1
over = " waALZ"
times = ["sabah", "gunduz", "aksam"]

[[effects]]
type = "sprite"            # the vapur, about 29 s per crossing at 10 fps
art = {toml_lines(FERRY_ART)}
mask = {toml_lines(FERRY_MASK)}
row = {FERRY_ROW}
speed = 0.55
direction = -1
over = " waALZ"
smoke_key = "f"
"""

art, mask = cv.dump()
out = Path(sys.argv[1] if len(sys.argv) > 1 else "assets/scenes/vapur")
out.mkdir(parents=True, exist_ok=True)
(out / "large.art.txt").write_text(art)
(out / "large.mask.txt").write_text(mask)
(out / "scene.toml").write_text(SCENE)
print(art)
print("\n".join(FERRY_ART))
print("\n".join(FERRY_MASK))
