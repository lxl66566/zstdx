"""Print instructions [from, to) of a symbol in an asm file, with operands."""
import re
import sys

path, sym, lo, hi = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
LABEL = re.compile(r"^[^\s.][^:]*:$")
cur = None
i = 0
for line in open(path, encoding="utf8", errors="replace"):
    line = line.rstrip("\n")
    if LABEL.match(line) and line.startswith("_"):
        cur = line[:-1]
        continue
    if cur == sym and line.startswith("\t"):
        if lo <= i <= hi:
            print(f"{i:5} {line.strip()}")
        i += 1
