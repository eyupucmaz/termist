"""One-off authoring aid: composes the Galata large scene into art + mask text files."""
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

    def put_solid(self, row, col, text, key):
        for i, ch in enumerate(text):
            if ch != " ":
                self.put(row, col + i, ch, key)

    def putm(self, row, col, text, keys):
        """text with a same-length per-char key string."""
        assert len(text) == len(keys), (text, keys)
        for i, (ch, k) in enumerate(zip(text, keys)):
            if k == ".":  # transparent: keep what's underneath
                continue
            self.put(row, col + i, ch, k)

    def dump(self):
        return ("\n".join("".join(r).rstrip() for r in self.art) + "\n",
                "\n".join("".join(r).rstrip() for r in self.mask) + "\n")


cv = Canvas()

# --- water (rows 22..25) -------------------------------------------------
waves = ["~   ~~    ~  ", "  ~    ~~~   ", "~~   ~     ~ ", "   ~~   ~   ~"]
for i, pat in enumerate(waves):
    row = 22 + i
    line = (pat * 10)[:W]
    cv.put(row, 0, line, "w")

# --- quay (row 21) -------------------------------------------------------
cv.put(21, 0, "_" * 68, "G")

# --- Galata Tower, centre column c ---------------------------------------
c = 30
cv.put(0, c, "|", "D")                      # finial
cv.put(1, c, "^", "R")                      # apex
for k in range(1, 8):                      # cone rows 2..8
    row = 1 + k
    cv.put(row, c - k, "/", "R")
    cv.put(row, c - k + 1, " " * (2 * k - 1), "r")
    cv.put(row, c + k, "\\", "R")
cv.put(9, c - 8, "[" + "=" * 15 + "]", "D")  # railing
arches = " n n n n n n n "
cv.putm(10, c - 8, "|" + arches + "|", "D" + "".join("W" if ch == "n" else "s" for ch in arches) + "D")
cv.put(11, c - 8, "[" + "=" * 15 + "]", "D")
cv.put(12, c - 7, "\\" + "_" * 13 + "/", "D")  # cornice
body = [
    "           ",
    "  n     n  ",
    "           ",
    "     n     ",
    "           ",
    "  n     n  ",
    "           ",
]
for i, inner in enumerate(body):
    row = 13 + i
    cv.put(row, c - 6, "|", "S")
    cv.putm(row, c - 5, inner, "".join("W" if ch == "n" else "s" for ch in inner))
    cv.put(row, c + 6, "|", "S")

# --- houses on the slope ---------------------------------------------------
def house_a(col, base):
    cv.put_solid(base - 3, col, " ___ ", "T")
    cv.put(base - 2, col, "/___\\", "T")
    cv.putm(base - 1, col, "|n n|", "HWhWH")
    cv.putm(base, col, "|___|", "HHHHH")


def house_b(col, base):
    cv.put_solid(base - 4, col, " _____ ", "T")
    cv.put(base - 3, col, "/_____\\", "T")
    cv.putm(base - 2, col, "|n n n|", "HWhWhWH")
    cv.putm(base - 1, col, "|n n n|", "HWhWhWH")
    cv.putm(base, col, "|_____|", "HHHHHHH")


def house_c(col, base):  # tall narrow
    cv.put_solid(base - 5, col, "  _  ", "T")
    cv.put_solid(base - 4, col, " /_\\ ", "T")
    cv.putm(base - 3, col, "|n n|", "HWhWH")
    cv.putm(base - 2, col, "|n n|", "HWhWH")
    cv.putm(base - 1, col, "|n n|", "HWhWH")
    cv.putm(base, col, "|___|", "HHHHH")


for fn, col in [(house_a, 1), (house_c, 7), (house_b, 13), (house_a, 41),
                (house_b, 47), (house_c, 55), (house_a, 61),
                # in front of the tower: it stands up the hill, not on the quay
                (house_a, 21), (house_b, 26), (house_a, 33)]:
    fn(col, 20)

# --- sun / moon ----------------------------------------------------------
cv.put(2, 80, " .--. ", "M")
cv.put(3, 80, "(    )", "M")
cv.put(4, 80, " '--' ", "M")

# --- vapur on the water --------------------------------------------------
vapur = [
    ("      _||_      ", "......FFFF......"),
    ("  ___|____|___  ", "..VVVVVVVVVVVV.."),
    (" |  o  o  o  o| ", ".VVVWVVWVVWVVWV."),
    ("  \\__________/  ", "..KKKKKKKKKKKK.."),
]
for i, (t, k) in enumerate(vapur):
    cv.putm(19 + i, 74, t, k)

art, mask = cv.dump()
out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
(out / "large.art.txt").write_text(art)
(out / "large.mask.txt").write_text(mask)
print(art)
