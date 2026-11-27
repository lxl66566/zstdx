# Corpus and reproducibility


- Corpus comparability and cross-session drift pitfalls live in [benchmark methodology](../../bench/methodology.md). That covers the text.raw shape transitions and the same-session A/B rule.
## Published-crate test portability


Every fixture access goes through `fixture_entries` and `fixture_bytes`, for corpus and dictionary directories, single corpus files and fuzz artifacts in `tests/`. They skip with a note on `NotFound` but still panic on real IO errors, so the standalone `.crate` `cargo test` passes with no untracked corpus present while genuine breakage still fails. Bypassing them with a direct `Path` read reintroduces the corpus-not-in-git dependency at publish time.
## Windows autocrlf rewrites text-detected fixtures


Without a `.gitattributes`, fixture byte-exactness rested on git's text auto-detection. GitHub Windows runners default to `core.autocrlf=true`, and checkout inflated every text-detected fixture with CRLF: the 4 decodecorpus originals `abc.txt`, `z000040`, `z000056` and `z000079` against their binary-detected `.zst` frames, all 207 `dict_tests/files/*.service` training inputs, and the text-detected fuzz artifact `encode/crash-f338a0e3…`. `test_decode_corpus_files` failed WrongSize on exactly those 4 pairs with checksums clean, because only the original side of each pair changed bytes.

The fixture directory families are pinned with the `binary` macro in `.gitattributes`, which overrides autocrlf and machine-global `* text=auto` rules, so every checkout yields identical fixture bytes. Simulate a Windows checkout with `GIT_CONFIG_GLOBAL=/dev/null GIT_ATTR_GLOBAL=/dev/null git clone -c core.autocrlf=true <repo> /tmp/sim`, then byte-diff every tracked file: source files legitimately differ by CRLF, fixture directories must stay identical.
