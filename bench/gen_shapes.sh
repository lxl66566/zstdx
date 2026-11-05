#!/bin/bash
# New-shape corpus for the deep-offset ramp's cost-taxonomy validation
# (docs/src/dev/bench/shapes.md): 32 MiB (33554432 B) deterministic shapes
# outside the 6-shape calibration set, used to ground proposal 9's arming
# screens. No network. Output under bench/shapes is gitignored.
#
# Recipes (exact):
#   src.raw         tar (GNU format, --sort=name, --mtime=@0, owner/group 0)
#                   of the /nix/store *-source trees in sorted order, enough
#                   trees for 48 MiB apparent size, truncated at 32 MiB.
#                   Reruns after a store update differ, like gen_big.sh's
#                   ELF aggregate.
#   html.raw        deterministic HTML crawl: ~4 KiB pages, fixed head/nav/
#                   footer boilerplate per page, body prose from a seeded
#                   vocabulary (seed 11).
#   md.raw          markdown document mix: headings, prose paragraphs, bullet
#                   lists, fenced code blocks and tables from a seeded
#                   vocabulary (seed 12).
#   csv.raw         CSV export: fixed header, then records of
#                   id,hash,name,category,created_at,score_a,score_b,status,
#                   quoted description (seed 13).
#   fasta.raw       DNA reads: ">gnl|db|taxon|ID motif=..." headers plus
#                   60-char sequence lines over ACGTN, built as mutated
#                   motif copies so reads share local structure (seed 14).
#   snapshot.raw    periodic binary snapshots: a 1 MiB structured binary
#                   frame (256-B records: u32 id, u64 timestamp, two f32
#                   fields, 200 B high-entropy payload, 34 B entropy
#                   patch) whose frame k+1 is frame k with ~0.7% of bytes
#                   bit-flipped in place (seed 15). The dominant match
#                   offset sits at exactly 1 MiB with a 256-symbol
#                   alphabet: the wide-alphabet/period-in-band stress case
#                   for the distinct-density screen.
#   mixed.raw       24 MiB of bench/corpus/json.raw plus 8 MiB of seeded
#                   prose (seed 16), one file: the screen-dilution payload
#                   with a benign tail and a job-aligned region boundary.
#                   Needs bench/gen_corpus.sh first.
#   mixed_text.raw  22 MiB of bench/corpus/json.raw plus 10 MiB of
#                   bench/corpus/text.raw (the ~800 KiB-tile tiled shape):
#                   dilution with a ramp-hostile tail whose region boundary
#                   at 22 MiB falls mid-job on the 32 MiB level-3 job
#                   lattice. Needs bench/gen_corpus.sh first.
# Usage: bash bench/gen_shapes.sh
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p bench/shapes
SZ=33554432

# src: tarball-shaped aggregate of real source trees
if [ ! -f bench/shapes/src.raw ]; then
	roots=()
	total=0
	while read -r d; do
		roots+=("$d")
		total=$((total + $(du -sb "$d" 2>/dev/null | cut -f1)))
		[ $total -ge $((48 * 1024 * 1024)) ] && break
	done < <(find /nix/store -maxdepth 1 -name "*-source" -type d 2>/dev/null | sort)
	tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
		-cf bench/shapes/.src.tar "${roots[@]}" 2>/dev/null || true
	head -c $SZ bench/shapes/.src.tar > bench/shapes/src.raw
	rm -f bench/shapes/.src.tar
fi

# html: boilerplate-heavy crawl pages
if [ ! -f bench/shapes/html.raw ]; then
	python3 - <<'EOF'
import random
random.seed(11)
SZ = 33554432
WORDS = ["data","system","report","value","service","update","record","query",
         "index","state","event","user","config","model","process","metric",
         "server","client","node","cache","stream","buffer","table","field",
         "request","response","handler","schema","format","version","token",
         "policy","session","account","profile","message","channel","group",
         "entry","entity","object","class","method","frame","packet","route"]
TAGS = ["div","span","section","article","header","footer","nav","aside","main"]
attrs = ["id","class","data-id","data-ref","role","title"]
css = ("<style>body{margin:0;padding:0;font-family:sans-serif}.layout{display:"
       "grid;grid-template-columns:auto 1fr}.menu{width:220px}.content{"
       "padding:16px}.footer{clear:both}</style>")
nav = ('<nav class="menu"><a href="/index.html">Home</a><a href="/list.html">'
       'All records</a><a href="/about.html">About</a><a href="/help/q1.html">'
       'Help</a></nav>')
out = open("bench/shapes/html.raw", "w")
page = 0
while out.tell() < SZ:
    body = []
    for _ in range(random.randint(4, 9)):
        body.append("<p>" + " ".join(
            random.choice(WORDS) for _ in range(random.randint(20, 70))) +
            ".</p>")
        if random.random() < 0.4:
            t = random.choice(TAGS)
            a = " ".join('%s="%s%d"' % (random.choice(attrs),
                                         random.choice(WORDS),
                                         random.randint(0, 9999))
                         for _ in range(random.randint(1, 3)))
            body.append("<%s %s>%s %d</%s>" % (t, a, random.choice(WORDS),
                                               random.randint(0, 999), t))
    links = "".join('<a href="/rec/%d.html">%s %d</a>' %
                    (random.randint(0, 99999), random.choice(WORDS),
                     random.randint(0, 999)) for _ in range(6))
    out.write("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">"
              "<title>%s %d</title><meta name=\"viewport\" content=\"width="
              "device-width\"><link rel=\"stylesheet\" href=\"/site.css\">"
              "%s</head><body><header class=\"top\"><h1>Site: %s</h1></header>"
              "<div class=\"layout\">%s<main class=\"content\">%s<p>%s</p>"
              "</main></div><footer class=\"footer\">Generated dump page %d"
              "</footer></body></html>\n" %
              (random.choice(WORDS), page, css, random.choice(WORDS), nav,
               "".join(body), links, page))
    page += 1
out.close()
import os
os.truncate("bench/shapes/html.raw", SZ)
EOF
fi

# md: prose and structure mix
if [ ! -f bench/shapes/md.raw ]; then
	python3 - <<'EOF'
import random
random.seed(12)
SZ = 33554432
NOUNS = ["pipeline","window","buffer","sequence","offset","header","block",
         "table","candidate","chain","search","budget","level","frame",
         "stream","pool","worker","job","strip","segment","entropy","tree",
         "match","literal","codec","ratio","speed","cache","branch","cost"]
VERBS = ["reads","writes","keeps","drops","arms","rejects","holds","prices",
         "settles","adopts","clears","resolves","returns","covers","moves"]
ADJ = ["fast","wide","narrow","cold","hot","deep","shallow","dense","sparse",
       "ramped","stock","armed","late","early","local","global"]
def sentence():
    return "%s %s the %s %s" % (random.choice(NOUNS).capitalize(),
                                random.choice(VERBS),
                                random.choice(ADJ),
                                random.choice(NOUNS))
def paragraph():
    return " ".join(sentence() + "." for _ in range(random.randint(3, 8)))
def code_block():
    lines = []
    for _ in range(random.randint(3, 12)):
        lines.append("let %s = %s_%d(%s);" % (
            random.choice(NOUNS), random.choice(VERBS),
            random.randint(0, 999), ", ".join(
                random.sample(NOUNS, random.randint(1, 3)))))
    return "```rust\n" + "\n".join(lines) + "\n```"
out = open("bench/shapes/md.raw", "w")
sec = 0
while out.tell() < SZ:
    out.write("# Section %d: %s\n\n%s\n\n" %
              (sec, sentence().rstrip("."), paragraph()))
    for _ in range(random.randint(2, 6)):
        kind = random.random()
        if kind < 0.35:
            out.write("## %s\n\n%s\n\n" % (sentence().rstrip("."),
                                            paragraph()))
        elif kind < 0.6:
            out.write("- %s\n- %s\n- %s\n\n" % (sentence(), sentence(),
                                                 sentence()))
        elif kind < 0.8:
            out.write(code_block() + "\n\n")
        else:
            out.write("| column | value |\n|---|---|\n")
            for _ in range(random.randint(2, 8)):
                out.write("| %s | %d |\n" % (sentence(),
                                              random.randint(0, 99999)))
            out.write("\n")
    sec += 1
out.close()
import os
os.truncate("bench/shapes/md.raw", SZ)
EOF
fi

# csv: tabular export records
if [ ! -f bench/shapes/csv.raw ]; then
	python3 - <<'EOF'
import random
random.seed(13)
SZ = 33554432
CATS = ["alpha","beta","gamma","delta","epsilon","zeta","eta","theta"]
STATUS = ["new","open","pending","blocked","done","archived","failed"]
NAMES = ["korva","linden","aster","quill","marrow","talon","ferro","sable",
         "orchid","basalt","cinder","delta","ember","fjord","granite"]
def name():
    return "%s-%s%d" % (random.choice(NAMES), random.choice(NAMES),
                        random.randint(0, 9999))
def desc():
    return " ".join(random.choice(CATS) for _ in range(3, 12))
out = open("bench/shapes/csv.raw", "w")
out.write("id,ref_hash,name,category,created_at,score_a,score_b,status,"
          "description\n")
i = 0
while out.tell() < SZ:
    out.write("%d,%08x%08x,%s,%s,2026-01-%02dT%02d:%02d:%02dZ,%.4f,%.4f,"
              "%s,\"%s\"\n" %
              (i, random.getrandbits(32), random.getrandbits(32), name(),
               random.choice(CATS), random.randint(1, 28),
               random.randint(0, 23), random.randint(0, 59),
               random.randint(0, 59), random.uniform(-1, 1),
               random.uniform(-1, 1), random.choice(STATUS), desc()))
    i += 1
out.close()
import os
os.truncate("bench/shapes/csv.raw", SZ)
EOF
fi

# fasta: mutated-motif DNA reads
if [ ! -f bench/shapes/fasta.raw ]; then
	python3 - <<'EOF'
import random
random.seed(14)
SZ = 33554432
MOTIFS = ["".join(random.choice("ACGTN") for _ in range(random.randint(8, 24)))
          for _ in range(64)]
out = open("bench/shapes/fasta.raw", "w")
sid = 0
while out.tell() < SZ:
    out.write(">gnl|db|taxon%d|SEQ%08d motif=%d desc=read sequence sample\n"
              % (sid % 97, sid, sid % 64))
    seq = []
    while sum(len(s) for s in seq) < 9000:
        m = random.choice(MOTIFS)
        seq.append("".join(random.choice("ACGT")
                           if random.random() < 0.004 else ch
                           for ch in m))
    flat = "".join(seq)
    for j in range(0, len(flat), 60):
        out.write(flat[j:j + 60] + "\n")
    sid += 1
out.close()
import os
os.truncate("bench/shapes/fasta.raw", SZ)
EOF
fi

# snapshot: periodic structured-binary deltas (wide alphabet, 1 MiB period)
if [ ! -f bench/shapes/snapshot.raw ]; then
	python3 - <<'EOF'
import random, struct
random.seed(15)
SZ = 33554432
FRAME = 1 << 20
def base_frame():
    b = bytearray(FRAME)
    i = 0
    while i < FRAME:
        # record: u32 id, u64 ts, f32 x, f32 y (22 B), 200 B high-entropy
        # payload, 34 B entropy patch widening the head beyond ASCII
        rec = struct.pack("<IQff", i, 1700000000 + i, random.uniform(0, 1),
                          random.uniform(0, 1))
        b[i:i + len(rec)] = rec
        b[i + 22:i + 222] = bytes(random.getrandbits(8)
                                  for _ in range(200))
        for j in range(i + 222, i + 256):
            b[j] = random.getrandbits(8)
        i += 256
    return b
frame = base_frame()
out = open("bench/shapes/snapshot.raw", "wb")
for _ in range(SZ // FRAME + 1):
    out.write(frame)
    # next frame: same content, ~0.7% of bytes bit-flipped in place
    for _ in range(FRAME * 7 // 1000):
        frame[random.randrange(FRAME)] ^= 1 << random.randrange(8)
out.close()
import os
os.truncate("bench/shapes/snapshot.raw", SZ)
EOF
fi

# mixed payloads: need the base corpus first
for f in json text; do
	[ -f "bench/corpus/$f.raw" ] || { echo "run bench/gen_corpus.sh first" >&2; exit 1; }
done

# prose tail for the mixed payload (pure prose, no markup; seed 16)
if [ ! -f bench/shapes/.prose8.raw ] && [ ! -f bench/shapes/mixed.raw ]; then
	python3 - <<'EOF'
import random, os
random.seed(16)
SZ = 8 * 1024 * 1024
NOUNS = ["pipeline","window","buffer","sequence","offset","header","block",
         "table","candidate","chain","search","budget","level","frame",
         "stream","pool","worker","job","strip","segment"]
VERBS = ["reads","writes","keeps","drops","arms","rejects","holds","prices",
         "settles","adopts","clears","resolves","returns","covers","moves"]
ADJ = ["fast","wide","narrow","cold","hot","deep","shallow","dense","sparse"]
out = open("bench/shapes/.prose8.raw", "w")
while out.tell() < SZ:
    out.write(" ".join("%s %s the %s %s." %
                       (random.choice(NOUNS).capitalize(),
                        random.choice(VERBS), random.choice(ADJ),
                        random.choice(NOUNS))
                       for _ in range(random.randint(4, 9))) + "\n")
out.close()
os.truncate("bench/shapes/.prose8.raw", SZ)
EOF
fi

# mixed: 24 MiB json + 8 MiB prose, job-aligned boundary
if [ ! -f bench/shapes/mixed.raw ]; then
	{ dd if=bench/corpus/json.raw bs=1M count=24 status=none
	  cat bench/shapes/.prose8.raw
	} > bench/shapes/mixed.raw
fi

# mixed_text: 22 MiB json + 10 MiB tiled text, mid-job boundary
if [ ! -f bench/shapes/mixed_text.raw ]; then
	{ dd if=bench/corpus/json.raw bs=1M count=22 status=none
	  dd if=bench/corpus/text.raw bs=1M count=10 status=none
	} > bench/shapes/mixed_text.raw
fi

rm -f bench/shapes/.prose8.raw
for f in bench/shapes/*.raw; do
	s=$(stat -c%s "$f")
	[ "$s" -eq "$SZ" ] || { echo "$f: $s bytes, expected $SZ" >&2; exit 1; }
done
ls -l bench/shapes/
