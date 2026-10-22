# R66 review — dictionary-frame decode parity (todo 21a)

Scope: the decode paths only reachable with a dictionary attached — the ring-buffer executor (`decoding/sequence_execution.rs::execute_sequences` + `decoding/decode_buffer.rs`'s `repeat_from_dict`/`NotEnoughBytesInDictionary`/`OffsetTooBig` arms, entered because `flat_active = false` on dictionary registration in `frame_decoder.rs`), the dictionary load (`decoding/dictionary.rs`, `encoding/dictionary.rs::EncDictionary::parse`), and the dictID frame/header classes (declared id, suppressed id, id-0 dict). Reference: libzstd v1.5.7 CLI (`zstd -d -D <dict> -c`) and `ZSTD_*` in /root/programs/fork/zstd/lib. Extends R65's [parity](parity.md) method to the dict-only surface.

Method: a scratch differential harness (not committed; todo 21b institutionalizes the shape) comparing, per case, `bulk::decompress_with(frame, .., DecoderOptions::new().dictionary(dict))` against the CLI — accept/reject plus full output bytes on accepts, panics counted as failures (`catch_unwind`, none occurred). Dictionary-load acceptance is isolated from frame decoding by decoding a valid empty frame (9 B: magic + FHD 0 + window descriptor 0 + raw last block 0) under `-D <dict>`, so a CLI failure there is exactly a load failure, compared against `Dictionary::load`.

Corpus: 13 dictionary forms — four formatted (`zstd --train` over 120-file text/binary/small sample sets; maxdict 3 KiB for one; one with `--dictID=0`), one raw content (41 KiB text), five crafted-rep (the repcode tail rewritten to [c,c,c], [c,1,1], [1,1,1], [c−1,2,1] on the small dict and [c,c,c] on the 110 KiB text dict — all legal at load, wildly different as seeded offset history), three tiny raw (4- and 7-byte magic-prefixed, 1 byte). 265 frames: the 5×6×4 level grid (1/3/9/19 on text/rle/binary/tiny/empty/dict-repeat payloads), eight window-boundary variants (`--long=10`: 1 KiB windows under 4-31 KiB payloads, total output crossing the window mid-frame, checksum variant included), eleven repcode-targeted (payloads whose first match sits at rep1/rep3 distance from the dict content end), six tiny-dict, and 120 zstdx-encoded with `include_dictid(false)` — the null-id decode class (libzstd applies a loaded dictionary unconditionally; so do the options-based entries via the id-0 registration).

Mutations: per-position truncation ladder (exhaustive up to 400 B, dense pass exhaustive to 1000 B), single-byte XOR 0xFF/0x01/0x80 (exhaustive on small frames, sampled on large), AND 0xFE / OR 0x03 / OR 0x80 on the first 24 bytes (frame header, descriptor, dictID field), and dictionary-side: truncations, every-byte XOR 0xFF/0x01 on the 3 KiB dict, XOR sweeps of the entropy-table head and the 12 repcode tail bytes, AND/OR on magic+id. Crafted-rep dicts were additionally mutated in their rep tail (XOR 0x01/0x0F) and decoded against repcode-using frames, so the dict-seeded `do_offset_history` resolution is compared byte-for-byte on the deep-rep shapes (rep = content, rep = content−1, all-ones), not just accepted.

Case counts: 203,016 at discovery (66,114 sampled + 124,395 dense + 12,507 craft), 204,374 after the fix (67,472 + 124,395 + 12,507), zero divergences beyond the classes below. Arm coverage was verified by an in-process pass (1.38 M decode calls, no CLI): `NotEnoughBytesInDictionary` 164, `OffsetTooBig` 1, `BlockOutputTooLarge` 21, plus the shared executor arms (`NotEnoughBytesForSequence` 130, `ZeroOffset`, checksum/literals/FSE/Huffman classes) — the dict-only arms are exercised, not just reachable.

## Divergences found

| # | Class | Finding | Verdict |
|---|---|---|---|
| 1 | rejects-what-accepts | A 4..7-byte dictionary starting with the dictionary magic: `Dictionary::load` routed any magic-prefixed input to the formatted parser, which rejects under 8 bytes (`NotEnoughBytes`), while libzstd's auto content type treats `dictSize < 8` as pure content (`ZSTD_loadEntropy_intoDDict`) — the CLI both encodes and decodes with such a dict as 4-7 bytes of raw history (76 RWAL cases over a libzstd-made 31 B frame, plus every DICTLOAD probe). The encode-side `EncDictionary::parse` had the same routing. | **Fixed** — both loads take `len < 8` as raw content (id 0, format-default repcodes [1,4,8], the bytes themselves as history); `decode_dict` (the explicit formatted parser) stays strict. Pinned by `short_magic_prefixed_dictionary_loads_as_raw_content` (all lengths 0..8, plus the 8-byte boundary rejecting as formatted), `frame_compressed_with_short_magic_dict_decodes` (the libzstd frame bytes embedded), and `EncDictionary`'s `short_magic_prefixed_dict_parses_as_raw_content` (encode side); the compat fixtures (and the CLI's `failed_compress_removes_partial_output`) now use a genuinely-broken dict. 48bb868 |
| 2 | not a divergence | Empty input (truncation to 0 bytes): zstdx's one-shot `decompress` returns `Ok(empty)` where the CLI rejects — but libzstd's one-shot `ZSTD_decompress` on srcSize 0 also returns 0 with no error (C probe against v1.5.7); the CLI failure is the streaming file layer demanding a frame. The same one-shot/streaming pairing class as R65's finding 7. | **Documented**, no action |

## Verified clean (no action)

- Ring-executor dict arms: `repeat_from_dict` boundary algebra (offset = content, content±1, dict-spanning matches, `bytes_from_dict` split into the in-ring continuation) — byte-identical to the reference on crafted-rep dictionaries and their rep-tail mutations across all repcode slots, including the deep-rep [c,c,c] dict and the rep0−1 pseudo-slot saturation guard (#115's class).
- Window boundary: `--long=10` frames (1 KiB window, payloads 4-31 KiB) under exhaustive per-byte mutation — the dict-reachability cutoff (`total_output ≤ window_size` in zstdx vs libzstd's `checkContinuity`/virtualStart folding at flush edges) never diverges; both the accepted outputs and the rejection points agree.
- Dictionary-load parity: every truncation and byte mutation of the formatted dicts — a load the reference refuses, zstdx refuses (bad entropy tables, truncated tables, repcodes past content, repcode 0); a load the reference accepts (including `dictSize ≤ 8` auto-content and magic-mismatch raw), zstdx accepts post-fix.
- Decoding under mutated-but-loading dicts: entropy-table mutations that still parse (repaired-ish tables, shifted content) decode the valid frame identically on both sides — same accept/reject, same output bytes.
- DictID classes: declared-id frames under the matching dict (both decode), under a wrong-id dict (both reject: `DictNotProvided` vs `dictionary_wrong`), suppressed-id frames under the options/null-id registration (both decode; the include_dictid(false) interop class), id-0 dicts on id-less frames, and 8-byte magic-prefixed dicts (both reject at the formatted parse).
- Shared arms under dict load: block caps (`check_block_output_size`, `BlockOutputTooLarge` at `min(window, 128 KiB)`), literals/sequence budget errors, checksum mismatch with dictionary-seeded history, content-size verification — all agree, consistent with R65's dict-less findings since the parser front end is shared.

## Fuzz (todo 21d)

`huff0.rs` gained `weight_alphabet_cap_seed`: the R65 alphabet-cap class as a constructed seed — a weights NCount at accuracy log 6 declaring symbol 12/13 with one state the emitted weights never use (emitted alphabet 0..=2), so no weight-set bound can reject it and only `HuffmanTable`'s `FSETable(11)` cap fires; the declared-11 control must pass the cap. Proof: `cargo fuzz run --fuzz-dir crates/zstdx-fuzz huff0 -s none -- -runs=2000` completes, no findings. 7e07bdc

## Commits

- 48bb868 decoding: load <8-byte dictionaries as raw content like libzstd
- 7e07bdc fuzz: seed the maxSV>=12 declared-never-emitted weights header in huff0

## Tests

Full workspace `cargo test` (debug, 395 zstdx lib tests incl. the 3 new pins), `--release -p zstdx`, `-p zstdx --no-default-features`: green.
