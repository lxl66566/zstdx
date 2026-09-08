"""Compare two rustc-emitted asm files at the instruction-stream level.

For every global label, extract the sequence of instruction mnemonics and
compare. Immediates, operands, label renumbering and symbol-name hashes are
ignored, so a match means both files run the same instruction sequence.
Usage: python asm_semantic_diff.py BASE.s NEW.s
"""
import re
import sys

LABEL = re.compile(r"^[^\s.][^:]*:$")


def load(path):
    funcs = {}
    cur = None
    buf = []
    for line in open(path, encoding="utf8", errors="replace"):
        line = line.rstrip("\n")
        if LABEL.match(line) and line.startswith("_"):
            # only mangled global symbols; anon.* / .L* / ?dtor stay inline
            if cur is not None:
                funcs[cur] = buf
            cur = line[:-1]
            buf = []
            continue
        if line.startswith("\t"):
            tok = line.strip().split(None, 1)
            if tok and not tok[0].endswith(":"):
                buf.append(tok[0])
    if cur is not None:
        funcs[cur] = buf
    return funcs


def main():
    a = load(sys.argv[1])
    b = load(sys.argv[2])

    only_a = set(a) - set(b)
    only_b = set(b) - set(a)
    for s in list(only_a)[:5]:
        print("  -", s[:110])
    for s in list(only_b)[:5]:
        print("  +", s[:110])

    common = a.keys() & b.keys()
    diff = [s for s in common if a[s] != b[s]]
    print(
        f"{len(only_a)} removed, {len(only_b)} added, "
        f"{len(common)} common symbols, {len(diff)} differ"
    )
    for s in diff[:20]:
        print("  !", s[:130])
        for i, (x, y) in enumerate(zip(a[s], b[s])):
            if x != y:
                print(f"     at instruction {i}: {x} vs {y}")
                break
        else:
            print(f"     length {len(a[s])} vs {len(b[s])}")


if __name__ == "__main__":
    main()
