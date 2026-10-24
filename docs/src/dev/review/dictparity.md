# Dictionary-frame decode parity

Scope: the decode paths only reachable with a dictionary attached. The ring-buffer executor runs `decoding/sequence_execution.rs::execute_sequences` and the `repeat_from_dict`, `NotEnoughBytesInDictionary` and `OffsetTooBig` arms in `decoding/decode_buffer.rs`, which are entered because `flat_active = false` on dictionary registration in `frame_decoder.rs`. Dictionary load lives in `decoding/dictionary.rs` and `encoding/dictionary.rs::EncDictionary::parse`. The dictID frame and header classes are the declared id, the suppressed id and the id-0 dict. Reference: the libzstd v1.5.7 CLI through `zstd -d -D <dict> -c`, and the `ZSTD_*` entry points in `/root/programs/fork/zstd/lib`. The method extends the [decode error paths](parity.md) differential to the dictionary-only surface.

## Method

A scratch differential harness, not committed, compares `bulk::decompress_with(frame, .., DecoderOptions::new().dictionary(dict))` against the CLI for every case. It compares accept or reject and the full output bytes on accepts, and counts panics as failures through `catch_unwind`. None occurred. Dictionary-load acceptance is isolated from frame decoding by decoding a valid empty frame of 9 B under `-D <dict>`. The 9 B are the magic, FHD 0, window descriptor 0 and a raw last block 0, so a CLI failure there is exactly a load failure, and the result is compared against `Dictionary::load`.

## Corpus

The corpus has 13 dictionary forms. Four are formatted by `zstd --train` over 120-file text, binary and small sample sets, with maxdict 3 KiB for one and `--dictID=0` for another. One is raw content of 41 KiB text. Five are crafted-rep, with the repcode tail rewritten to `[c,c,c]`, `[c,1,1]`, `[1,1,1]` and `[c-1,2,1]` on the small dict and to `[c,c,c]` on the 110 KiB text dict. Those are all legal at load and wildly different as seeded offset history. Three are tiny raw forms at 4 and 7 bytes magic-prefixed and at 1 byte.

The 265 frames form a 5x6x4 level grid at levels 1, 3, 9 and 19 over text, rle, binary, tiny, empty and dict-repeat payloads. Eight window-boundary variants use `--long=10` with 1 KiB windows under 4 to 31 KiB payloads, and their total output crosses the window mid-frame. One variant includes the checksum. Eleven repcode-targeted frames use payloads whose first match sits at rep1 or rep3 distance from the dict content end. Six frames use a tiny dictionary. The last 120 frames are zstdx-encoded with `include_dictid(false)`, which forms the null-id decode class. libzstd applies a loaded dictionary unconditionally, and the options-based entries do the same through the id-0 registration.

## Mutations

The per-position truncation ladder is exhaustive up to 400 B and has a dense pass that is exhaustive to 1000 B. Single-byte XOR uses 0xFF, 0x01 and 0x80, exhaustively on small frames and sampled on large ones. On the first 24 bytes, which hold the frame header, descriptor and dictID field, it applies AND 0xFE, OR 0x03 and OR 0x80. Dictionary-side mutations include truncations, every-byte XOR with 0xFF and 0x01 on the 3 KiB dict, XOR sweeps of the entropy-table head and the 12 repcode tail bytes, and AND and OR operations on the magic and id.

The crafted-rep dicts were additionally mutated in their rep tail with XOR 0x01 and 0x0F and decoded against repcode-using frames. This compares the dict-seeded `do_offset_history` resolution byte for byte on the deep-rep shapes of rep = content, rep = content-1 and all-ones, and does not stop at acceptance.

## Case counts

Discovery ran 203,016 cases: 66,114 sampled, 124,395 dense and 12,507 crafted. After the fix the run holds 204,374 cases: 67,472 sampled, 124,395 dense and 12,507 crafted. It found zero divergences beyond the classes below.

Arm coverage was verified by an in-process pass of 1.38 M decode calls without the CLI. It hit `NotEnoughBytesInDictionary` 164 times, `OffsetTooBig` once and `BlockOutputTooLarge` 21 times, plus the shared executor arms with `NotEnoughBytesForSequence` 130 times, `ZeroOffset`, and the checksum, literals, FSE and Huffman classes. The dictionary-only arms are exercised in these runs.

## Divergences found

A 4 to 7-byte dictionary starting with the dictionary magic was rejected. `Dictionary::load` routed any magic-prefixed input to the formatted parser, which rejects under 8 bytes with `NotEnoughBytes`. libzstd's auto content type treats `dictSize < 8` as pure content through `ZSTD_loadEntropy_intoDDict`, so the CLI both encodes and decodes with such a dict as 4 to 7 bytes of raw history. The case covered 76 RWAL cases over a libzstd-made 31 B frame plus every DICTLOAD probe. The encode-side `EncDictionary::parse` had the same routing.

Both loads now take `len < 8` as raw content with id 0, the format-default repcodes `[1,4,8]` and the bytes themselves as history. `decode_dict`, the explicit formatted parser, stays strict. `short_magic_prefixed_dictionary_loads_as_raw_content` pins all lengths 0..8 plus the 8-byte boundary rejecting as formatted. `frame_compressed_with_short_magic_dict_decodes` embeds the libzstd frame bytes. `EncDictionary`'s `short_magic_prefixed_dict_parses_as_raw_content` pins the encode side. The compat fixtures, and the CLI's `failed_compress_removes_partial_output`, now use a genuinely broken dict.

Empty input is not a divergence. zstdx's one-shot `decompress` returns `Ok(empty)` where the CLI rejects, but libzstd's one-shot `ZSTD_decompress` on srcSize 0 also returns 0 with no error, confirmed by a C probe against v1.5.7. The CLI failure is the streaming file layer demanding a frame. This is the same one-shot and streaming pairing class described in the [decode error paths](parity.md) review, and it needs no action.

## Verified clean

- The ring-executor dictionary arms are clean. `repeat_from_dict`'s boundary algebra covers offset = content, content+1, content-1, dict-spanning matches and the `bytes_from_dict` split into the in-ring continuation. It is byte-identical to the reference on crafted-rep dictionaries and their rep-tail mutations across all repcode slots, including the deep-rep `[c,c,c]` dict and the rep0-1 pseudo-slot saturation guard.
- The window boundary is clean. `--long=10` frames with a 1 KiB window and 4 to 31 KiB payloads pass exhaustive per-byte mutation. The dict-reachability cutoff, `total_output ≤ window_size` in zstdx against libzstd's `checkContinuity` and virtualStart folding at flush edges, never diverges. Both the accepted outputs and the rejection points agree.
- Dictionary-load parity holds. Every truncation and byte mutation of the formatted dicts is handled alike. A load the reference refuses, zstdx refuses: bad entropy tables, truncated tables, repcodes past content and repcode 0. A load the reference accepts, zstdx accepts after the fix, including `dictSize ≤ 8` auto-content and magic-mismatch raw.
- Decoding under mutated but loading dicts is clean. Entropy-table mutations that still parse, such as repaired tables and shifted content, decode the valid frame identically on both sides, with the same accept or reject and the same output bytes.
- The dictID classes agree. Declared-id frames decode under the matching dict, and both sides reject a wrong-id dict with `DictNotProvided` against `dictionary_wrong`. Suppressed-id frames decode under the options and null-id registration on both sides, which is the `include_dictid(false)` interop class. id-0 dicts work on id-less frames, and 8-byte magic-prefixed dicts are rejected at the formatted parse by both.
- The shared arms agree under dictionary load. These cover the block caps through `check_block_output_size` with `BlockOutputTooLarge` at `min(window, 128 KiB)`, the literals and sequence budget errors, a checksum mismatch with dictionary-seeded history, and content-size verification. The result is consistent with the dict-less findings in the [decode error paths](parity.md) review, because the parser front end is shared.

## Fuzz

`huff0.rs` gained `weight_alphabet_cap_seed`, which turns the alphabet-cap class from the previous round into a constructed seed. A weights NCount at accuracy log 6 declares symbol 12 or 13 with one state that the emitted weights never use, while the emitted alphabet is 0..=2. No weight-set bound can reject it, and only `HuffmanTable`'s `FSETable(11)` cap fires. The declared-11 control must pass the cap. `cargo fuzz run --fuzz-dir crates/zstdx-fuzz huff0 -s none -- -runs=2000` completes with no findings.

## Commits

- 48bb868 decoding: load <8-byte dictionaries as raw content like libzstd
- 7e07bdc fuzz: seed the maxSV>=12 declared-never-emitted weights header in huff0

## Verification

Full workspace `cargo test` was green with debug, `--release -p zstdx` and `-p zstdx --no-default-features`. The debug run had 395 zstdx lib tests, including the 3 new pins.
