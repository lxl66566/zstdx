#!/bin/bash
# Generate benchmark corpus into bench/corpus: raw data shapes + zstd-compressed
# variants at levels 1/3/9. Usage: bash bench/gen_corpus.sh
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p corpus
cd corpus

# text: source code and docs from this repo, tiled
if [ ! -f text.raw ]; then
    : > text.raw
    for i in $(seq 1 40); do
        cat ../../crates/zstdx/src/*.rs ../../crates/zstdx/src/*/*.rs ../../Readme.md ../../LICENSE >> text.raw 2>/dev/null || true
    done
    truncate -s 32M text.raw
fi

# zeros: maximally redundant
if [ ! -f zeros.raw ]; then
    head -c 32M /dev/zero > zeros.raw
fi

# json-ish: semi structured, moderate redundancy
if [ ! -f json.raw ]; then
    python3 - <<'EOF'
import random, json
random.seed(42)
with open("json.raw", "w") as f:
    for i in range(400000):
        obj = {
            "id": i,
            "user": "user_%d" % random.randint(0, 5000),
            "event": random.choice(["click", "view", "purchase", "error", "login"]),
            "ts": 1700000000 + i,
            "payload": "x" * random.randint(0, 24),
            "score": random.random(),
        }
        f.write(json.dumps(obj) + "\n")
        if f.tell() > 32 * 1024 * 1024:
            break
EOF
fi

# low-entropy random: random bytes from small alphabet
if [ ! -f skewed.raw ]; then
    python3 - <<'EOF'
import random
random.seed(7)
alphabet = bytes([random.randrange(256) for _ in range(16)])
with open("skewed.raw", "wb") as f:
    while f.tell() < 32 * 1024 * 1024:
        f.write(bytes(random.choice(alphabet) for _ in range(1 << 20)))
EOF
fi

# incompressible random
if [ ! -f random.raw ]; then
    head -c 32M /dev/urandom > random.raw
fi

for f in *.raw; do
    name="${f%.raw}"
    for lvl in 1 3 9; do
        if [ ! -f "$name.zst$lvl" ]; then
            zstd -q -$lvl -k -o "$name.zst$lvl" "$f"
        fi
    done
done
ls -la
