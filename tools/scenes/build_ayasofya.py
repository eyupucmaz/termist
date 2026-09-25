"""One-off authoring aid: composes the Ayasofya large scene into art + mask text files.

    python3 tools/scenes/build_ayasofya.py assets/scenes/ayasofya
"""
import sys
from pathlib import Path

W, H = 96, 26


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
            if k == ".":
                continue
            self.put(row, col + i, ch, k)

    def dump(self):
        return ("\n".join("".join(r).rstrip() for r in self.art) + "\n",
                "\n".join("".join(r).rstrip() for r in self.mask) + "\n")


def wins(inner, fill, win="W"):
    """Key string for a run of interior text: 'n' is a window, everything else is fill."""
    return "".join(win if ch == "n" else fill for ch in inner)


cv = Canvas()
C = 39            # centre column of the main dome
GROUND = 22       # ground line row (horizon key G)
BL, BR = C - 32, C + 32   # outer walls of the lower body
MAHYA = 5         # row of the front minarets' upper serefe, where the mahya hangs

# --- park: lawn, low wall and gravel path (rows 23..25) -------------------
cv.put(23, 0, (" ,    '    ,   .    ,  '     ,    .   '   ,    " * 3)[:W], "g")
cv.put(24, 0, ("[__]" * 24)[:W], "Q")
cv.put(25, 0, ("  .   .    .  .    .   .   " * 4)[:W], "e")
cv.put(GROUND, 0, "_" * W, "G")


# --- minarets --------------------------------------------------------------
def alem(row, col):
    """Gilded crescent on a short rod: crescent at `row`, rod just below."""
    cv.put(row, col, "C", "A")
    cv.put(row + 1, col, "|", "A")


def minaret_thick(x, top, base, balconies):
    """Sinan's pair: 5-wide shaft, pointed lead cap, serefe rings.
    x = left edge of shaft, top = row of the alem's crescent."""
    alem(top, x + 2)
    cv.putm(top + 2, x + 1, "/^\\", "RRR")
    cv.putm(top + 3, x, "/___\\", "RrrrR")
    for row in range(top + 4, base + 1):
        cv.putm(row, x, "|   |", "IiiiI")
    for b in balconies:
        cv.put(b, x - 1, "[=====]", "B")
    cv.putm(base, x, "|___|", "iiiii")


# all four are Sinan-style thick minarets, mirrored about the dome.
# rear pair first, standing lower (farther back): the semi-domes and body hide their shafts
minaret_thick(C - 30, 6, 17, [10])
minaret_thick(C + 26, 6, 17, [10])

# --- the great dome: a round hemisphere on its ring of forty windows -------
DOME = 8          # crown row; the alem's crescent sits just above it
cv.put(DOME - 1, C, "C", "A")
dome = [  # (row offset, left edge text, half-width to the outermost edge char)
    (0, "_.--''''", 8),
    (1, "_.-'", 11),
    (2, ".-'", 14),
    (3, ".'", 16),
    (4, "/", 17),
    (5, "(", 18),
]
for dy, edge, half in dome:
    inner = 2 * half + 1 - 2 * len(edge)
    mirror = edge[::-1].replace("/", "\\").replace("(", ")")
    if dy == 0:  # the crown is all edge, with the alem's rod rising from its middle
        cv.put(DOME, C - half, edge + "'" * inner + mirror, "U")
        cv.put(DOME, C, "|", "A")
        continue
    cv.putm(DOME + dy, C - half, edge + " " * inner + mirror,
            "U" * len(edge) + "u" * inner + "U" * len(edge))
DRUM = DOME + 6
drum = (" n" * 18)[:35]
cv.putm(DRUM, C - 18, "[" + drum + "]", "P" + wins(drum, "p") + "P")

# buttress towers flanking the dome
for bx in (C - 23, C + 19):
    cv.putm(DRUM - 1, bx, " /^\\ ", ".RRR.")
    cv.putm(DRUM, bx, "|___|", "XXXXX")
    for row in range(DRUM + 1, 18):
        cv.putm(row, bx, "|   |", "XxxxX")

# tympanum under the dome, between the buttresses: the great arch over an arcade
cv.put(DRUM + 1, C - 18, "[" + "=" * 35 + "]", "X")
tymp = ["  _.-'" + " ".join("n" * 12) + "'-._  ",
        "/ " + " ".join(["(n)"] * 8) + " \\"]
for i, inner in enumerate(tymp):
    assert len(inner) == 35, (i, len(inner))
    keys = "".join("W" if ch == "n" else ("X" if ch in "()_.-'/\\" else "p") for ch in inner)
    cv.putm(DRUM + 2 + i, C - 18, "|" + inner + "|", "P" + keys + "P")

# semi-domes stepping down to either side (west left, east right)
cv.putm(DRUM + 1, C - 27, "_.-'", "UUUU")
cv.putm(DRUM + 2, C - 31, "_.-'" + " " * 4, "UUUU" + "u" * 4)
cv.putm(DRUM + 3, C - 32, "[ n n n n", "P" + wins(" n n n n", "p"))
cv.putm(DRUM + 1, C + 24, "'-._", "UUUU")
cv.putm(DRUM + 2, C + 24, " " * 4 + "'-._", "u" * 4 + "UUUU")
cv.putm(DRUM + 3, C + 24, "n n n n ]", wins("n n n n ", "p") + "P")

# --- lower body --------------------------------------------------------------
cv.put(18, BL, "[" + "=" * (BR - BL - 1) + "]", "X")
# bays between buttress pilasters; the widest bay (under the dome) holds the gate
bays = [5, 8, 9, 13, 9, 8, 5]
patterns = {  # per bay width: one entry per body row (19..21)
    5: [" (n) ", "     ", "     "],
    8: [" (n)(n) ", "        ", "        "],
    9: [" (n) (n) ", "         ", "         "],
    13: ["    .---.    ", "   /     \\   ", "   |     |   "],
}


def body_keys(pat, door):
    keys = ""
    inside = False
    for ch in pat:
        if ch == "n":
            keys += "W"
        elif ch in "()./\\|-":
            keys += "X"
            if door and ch in "/\\|":
                inside = not inside
        else:
            keys += "d" if (door and inside) else "p"
    return keys


for i in range(3):
    line, keys = "|", "P"
    for j, bw in enumerate(bays):
        if j:
            line, keys = line + "|", keys + "X"
        pat = patterns[bw][i].ljust(bw)
        line += pat
        keys += body_keys(pat, bw == 13 and i >= 1)
    line, keys = line + "|", keys + "P"
    assert len(line) == BR - BL + 1, len(line)
    cv.putm(19 + i, BL, line, keys)
cv.put(GROUND, BL, "|" + "_" * (BR - BL - 1) + "|", "P")

# front (outer) pair, the tallest, stand on the ground either side of the body
minaret_thick(BL - 6, 1, GROUND, [MAHYA, 14])
minaret_thick(BR + 2, 1, GROUND, [MAHYA, 14])

# --- mahya: a string of lights hung between the front minarets' upper serefe, high
# above the dome's crown and alem, tied on just above the railings --------------------
x0, x1 = BL, BR
for x in range(x0, x1 + 1):
    row = MAHYA - 1 if min(x - x0, x1 - x) < 3 else MAHYA
    if cv.mask[row][x] == " " and x % 2 == 1:
        cv.put(row, x, "o" if x % 4 == 1 else ".", "L")

# --- sun / moon ---------------------------------------------------------------
cv.put(2, 84, " .--. ", "M")
cv.put(3, 84, "(    )", "M")
cv.put(4, 84, " '--' ", "M")


# --- Sultanahmet Park: trees and the fountain ------------------------------------
def tree(col, base):
    cv.putm(base - 4, col, "  .--.  ", "..YYYY..")
    cv.putm(base - 3, col, " (    ) ", ".YyyyyY.")
    cv.putm(base - 2, col, "(      )", "YyyyyyyY")
    cv.putm(base - 1, col, " '-..-' ", ".YYYYYY.")
    cv.putm(base, col, "   ||   ", "...tt...")


for col in (8, 63, 79, 88):
    tree(col, 23)

cv.putm(20, C - 1, "'.'", "jjj")          # the jet, in front of the gate
cv.putm(21, C - 1, " : ", ".j.")
cv.putm(22, C - 11, "_" * 11 + "|" + "_" * 11, "Q" * 11 + "j" + "Q" * 11)
cv.putm(23, C - 12, "(" + " ~" * 11 + " )", "Q" + "f" * 23 + "Q")

art, mask = cv.dump()
out = Path(sys.argv[1] if len(sys.argv) > 1 else "assets/scenes/ayasofya")
out.mkdir(parents=True, exist_ok=True)
(out / "large.art.txt").write_text(art)
(out / "large.mask.txt").write_text(mask)
print(art)
