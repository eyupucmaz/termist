"""One-off authoring aid: composes the Kiz Kulesi large scene into art + mask text files.

  python3 tools/scenes/build_kiz-kulesi.py assets/scenes/kiz-kulesi
"""
import sys
from pathlib import Path

W, H = 96, 26
HORIZON = 14            # first water row
SHORE = HORIZON - 2     # first filled row of the far shore
C = 30                  # tower centre column


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
        """text with a same-length per-char key string ('.' keeps what is underneath)."""
        assert len(text) == len(keys), (text, keys)
        for i, (ch, k) in enumerate(zip(text, keys)):
            if k != ".":
                self.put(row, col + i, ch, k)

    def dump(self):
        return ("\n".join("".join(r).rstrip() for r in self.art) + "\n",
                "\n".join("".join(r).rstrip() for r in self.mask) + "\n")


def keys_for(text, outline, fill, extra=None):
    """Per-char keys: spaces are fill, chars in `extra` get their own key, the rest outline."""
    extra = extra or {}
    return "".join(extra.get(ch, fill if ch == " " else outline) for ch in text)


cv = Canvas()

# --- water (rows HORIZON..25) ---------------------------------------------
waves = ["~    ~~      ~   ", "   ~      ~~~    ", " ~~     ~       ~", "     ~~    ~   ~ ",
         "~   ~~    ~  ", "  ~    ~~~   ", "~~   ~     ~ ", "   ~~   ~   ~"]
for row in range(HORIZON, H):
    pat = waves[(row - HORIZON) % len(waves)]
    if row < HORIZON + 3:           # far water: sparser marks
        pat = pat.replace("~~", "~ ")
    cv.put(row, 0, (pat * 12)[:W], "w")

# --- distant historic peninsula (faint, low contrast) ----------------------
# B = far outline (line art on the sky), b = far fill, Y = far city lights (lit from sunset)


def far_fill(row, c0, c1, lights=()):
    for col in range(c0, c1):
        cv.put(row, col, " ", "b")
    for col in lights:
        cv.put(row, col, ".", "Y")


def minaret(col, top):
    cv.put(top, col, "i", "B")
    for row in range(top + 1, SHORE):
        cv.put(row, col, "|", "B")


def dome(col, w):
    """A two-row far dome of width w centred on col, sitting on the shore fill."""
    half = w // 2
    top = "." + "-" * (w - 4) + "."
    mid = len(top) // 2
    cv.put(SHORE - 2, col - half + 1, top[:mid] + "^" + top[mid + 1:], "B")
    cv.putm(SHORE - 1, col - half, "/" + " " * (w - 2) + "\\", "B" + "b" * (w - 2) + "B")


# low shore: a thin strip on the calm left, a hill with mosques on the right
cv.put(SHORE, 0, "_" * 22, "B")
far_fill(SHORE + 1, 0, 44, lights=(4, 11, 19))
cv.put(SHORE - 1, 42, "_" * 47, "B")
cv.put(SHORE, 41, "/", "B")
far_fill(SHORE, 42, 89, lights=(48, 56, 64, 72, 80))
far_fill(SHORE + 1, 40, 96, lights=(52, 60, 68, 76, 84, 93))
cv.put(SHORE, 89, "\\", "B")
# Sultanahmet: dome and needle minarets
dome(50, 9)
for col, top in ((43, SHORE - 4), (45, SHORE - 5), (55, SHORE - 5), (57, SHORE - 4)):
    minaret(col, top)
# Ayasofya: one wide, low dome, stubbier minarets
dome(66, 11)
for col in (60, 72):
    minaret(col, SHORE - 4)
# Suleymaniye up the hill
dome(81, 9)
for col, top in ((76, SHORE - 5), (86, SHORE - 5)):
    minaret(col, top)
# Golden Horn gap, then a tiny Galata on the far right
cv.put(SHORE, 91, "_" * 5, "B")
cv.put(SHORE - 3, 93, "^", "B")
cv.put(SHORE - 2, 92, "/_\\", "B")
cv.put(SHORE - 1, 92, "| |", "B")

# --- Kiz Kulesi -------------------------------------------------------------
c = C
cv.put(0, c, "|", "D")                       # finial
cv.put(0, c + 1, ">", "Q")                   # the flag
cv.put(1, c, "^", "R")                       # small pointed lead cupola
cv.putm(2, c - 1, "/ \\", "RrR")
cv.putm(3, c - 2, "/   \\", "RrrrR")
cv.putm(4, c - 3, "(     )", "RrrrrrR")
cv.put(5, c - 3, "'-----'", "D")
cv.putm(6, c - 2, "|o|o|", "DLDLD")          # lantern
cv.putm(7, c - 4, "/       \\", "R" + "r" * 7 + "R")   # lead roof over the kosk
cv.putm(8, c - 5, "/_________\\", "R" + "r" * 9 + "R")
kosk = [" n  n  n ", " |  |  | "]           # tall arched windows
for i, inner in enumerate(kosk):
    row = 9 + i
    cv.put(row, c - 5, "|", "S")
    cv.putm(row, c - 4, inner, keys_for(inner, "W", "s"))
    cv.put(row, c + 5, "|", "S")
cv.put(11, c - 5, "[=========]", "D")       # balcony
cv.put(12, c - 4, "\\_______/", "D")          # corbels
# below the horizon the stone outline sits on the stone fill (U), not on the sky
for row, inner in ((13, "       "), (14, "   n   "), (15, "       ")):
    cv.put(row, c - 4, "|", "U")
    cv.putm(row, c - 3, inner, keys_for(inner, "W", "s"))
    cv.put(row, c + 4, "|", "U")
# low stone building around the foot of the tower
left, right = c - 15, c + 13
cv.put(16, left + 1, "_" * (c - 5 - left), "U")
cv.put(16, c - 4, "|", "U")
cv.putm(16, c - 3, "       ", "sssssss")
cv.put(16, c + 4, "|", "U")
cv.put(16, c + 5, "_" * (right - c - 5), "U")
for row in (17, 18):
    cv.put(row, left, "|", "U")
    cv.put(row, right, "|", "U")
    for col in range(left + 1, right):
        cv.put(row, col, " ", "s")
for col in (left + 3, left + 7, left + 11, c + 7, c + 11):
    cv.put(17, col, "n", "W")
    cv.put(18, col, "|", "W")
cv.put(17, c - 1, "_", "U")                  # door
cv.putm(18, c - 2, "| |", "UsU")
cv.put(19, left, "|" + "_" * (right - left - 1) + "|", "U")
# the rock islet: a solid mass under the building, ragged where it meets the water
# (P = rock edge drawn on water, p = rock fill), and a small landing stage (J)
rock_top = "_/" + "  .   ^    '    .   ^    '  . " + "\\_"
rl = c - len(rock_top) // 2
cv.putm(20, rl, rock_top, "PP" + "p" * (len(rock_top) - 4) + "PP")
rock_low = "^ ^^\\_/^^ ^\\__/^^^\\_/^ ^^"
cv.putm(21, rl + 2, rock_low, "".join("w" if ch == " " else "P" for ch in rock_low))
jl = rl + len(rock_top)
cv.put(20, jl, "=======", "J")
cv.putm(21, jl + 1, "|  |  |", "J..J..J")

# --- sun / moon ---------------------------------------------------------------
cv.put(2, 80, " .--. ", "M")
cv.put(3, 80, "(    )", "M")
cv.put(4, 80, " '--' ", "M")

art, mask = cv.dump()
out = Path(sys.argv[1] if len(sys.argv) > 1 else "assets/scenes/kiz-kulesi")
out.mkdir(parents=True, exist_ok=True)
(out / "large.art.txt").write_text(art)
(out / "large.mask.txt").write_text(mask)
print(art)
