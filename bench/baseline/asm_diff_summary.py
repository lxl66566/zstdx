"""Attribute asm diff hunks to the nearest preceding global symbol.

Usage: python asm_diff_summary.py BASE.s NEW.s
"""
import re
import subprocess
import sys
from collections import Counter

base, new = sys.argv[1], sys.argv[2]

with open(base, encoding="utf8", errors="replace") as f:
    a = f.readlines()

syms = {}
cur = "?"
for i, line in enumerate(a, 1):
    if line.startswith("_"):
        cur = line.split(":")[0]
    syms[i] = cur

d = subprocess.run(["diff", base, new], capture_output=True, text=True).stdout
c = Counter()
for line in d.splitlines():
    m = re.match(r"^(\d+)", line)
    if m:
        c[syms[int(m.group(1))]] += 1

for k, v in c.most_common(15):
    print(v, k)
