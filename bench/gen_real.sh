#!/bin/bash
# Real-world corpus shapes, 32 MiB each, fetched from pinned upstream
# archives (unlike gen_corpus.sh's synthetic generators). The two shapes are
# opt-in bench axes: they join a sweep only through an explicit
# `--shape jsonreal,textreal`, an empty selection keeps the core five, so
# every recorded sweep stays comparable. Output under bench/corpus is
# gitignored. Network is required only on first generation; existing files
# are never refetched, and the upstream bytes are verified by sha256.
#
# Recipes (exact):
#   jsonreal.raw   GitHub Archive event lines, one hour, first 32 MiB:
#                  http://data.gharchive.org/2019-01-01-12.json.gz
#                  15+ event schemas, nested objects, real prose inside
#                  issue comments and commit messages.
#   textreal.raw   English Wikipedia XML dump, first 32 MiB of enwik8:
#                  https://mattmahoney.net/dc/enwik8.zip
#                  real prose plus wiki markup, no tiled long-range period.
#   Both also get .zst1/3/9 variants via the zstd CLI, like the core shapes.
# Usage: bash bench/gen_real.sh
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p corpus
cd corpus
SZ=33554432

# pinned upstream bytes; verify before slicing so a silently changed
# upstream can never shift the corpus under a recorded bench
GH_SHA=fd3a1276f8e69e4de2dba745f863466d04bd2fe99955f543ead895f707759def
GH_URL=http://data.gharchive.org/2019-01-01-12.json.gz
WIKI_SHA=547994d9980ebed1288380d652999f38a14fe291a6247c157c3d33d4932534bc
WIKI_URL=https://mattmahoney.net/dc/enwik8.zip

fetch() { # fetch <url> <sha256> <dest>
	local url=$1 want_sha=$2 dest=$3
	if [ ! -s ".tmp.$dest" ]; then
		curl -sS --fail -o ".tmp.$dest" "$url"
	fi
	local got_sha
	got_sha=$(sha256sum ".tmp.$dest" | cut -d' ' -f1)
	[ "$got_sha" = "$want_sha" ] || {
		echo "$dest: sha256 $got_sha != pinned $want_sha (upstream changed)" >&2
		exit 1
	}
}

if [ ! -f jsonreal.raw ]; then
	fetch "$GH_URL" "$GH_SHA" gh.json.gz
	gzip -cd ".tmp.gh.json.gz" | head -c $SZ >.jsonreal.tmp
	mv .jsonreal.tmp jsonreal.raw
fi

if [ ! -f textreal.raw ]; then
	fetch "$WIKI_URL" "$WIKI_SHA" enwik8.zip
	python3 - "$SZ" <<'EOF'
import sys, zipfile
with zipfile.ZipFile(".tmp.enwik8.zip") as z, z.open("enwik8") as f:
    with open(".textreal.tmp", "wb") as o:
        o.write(f.read(int(sys.argv[1])))
import os
os.replace(".textreal.tmp", "textreal.raw")
EOF
fi

rm -f .tmp.gh.json.gz .tmp.enwik8.zip

for name in jsonreal textreal; do
	s=$(stat -c%s "$name.raw")
	[ "$s" -eq $SZ ] || { echo "$name.raw: $s bytes, expected $SZ" >&2; exit 1; }
	for lvl in 1 3 9; do
		if [ ! -f "$name.zst$lvl" ]; then
			zstd -q -$lvl -k -f -o "$name.zst$lvl" "$name.raw"
		fi
	done
done
ls -la jsonreal.* textreal.*
