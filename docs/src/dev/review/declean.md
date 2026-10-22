# R65 — decode-core cleanups (todo 20a/20c)

Small decode-core follow-ups from the R64 review round, each landed atomically. Reference for parity claims: libzstd v1.5.7 at `/root/programs/fork/zstd`.

## Task 1 — huff0 weight-table NCount alphabet cap (todo 20a)

**Verdict: done, cap at 11 — the todo's "12" was off by one.**

The R64 note said libzstd "caps the weight alphabet at 12 inside FSE_readNCount". Reading the reference: `HUF_readStats`' FSE branch calls `FSE_decompress_wksp_bmi2(huffWeight, hwSize-1, ip+1, iSize, 6, ...)`, and every call site (`HUF_readStats`' stack wksp, `HUF_readDTableX1/X2_wksp`'s `statsWksp`/`calleeWksp`) hands it a workspace of exactly `HUF_READ_STATS_WORKSPACE_SIZE_U32 = FSE_DECOMPRESS_WKSP_SIZE_U32(6, HUF_TABLELOG_MAX-1)`, i.e. sized for **maxSymbolValue 11**. Inside `FSE_decompress_wksp_body`, `readNCount` writes the actual charnum-1 into `maxSVPtr`, and the next line rejects `FSE_DECOMPRESS_WKSP_SIZE(tableLog, maxSV) > wkspSize` with `tableLog_tooLarge`. The formula is strictly increasing in maxSV above 11 (each extra symbol costs half a U32), so:

- weights NCount maxSV ≤ 11 → accepted;
- maxSV ≥ 12 → rejected (`tableLog_tooLarge`), *regardless of what the bitstream emits*.

The cap is an incidental workspace-sizing artifact, but deterministic on every decode path. Verified with a C probe against `libzstd.a` (`HUF_readStats`): crafted headers declaring a never-emitted symbol with probability 1 — symbol 11 ACCEPTED (nbSymbols 254, tableLog 8), symbols 12 and 13 REJECTED with `tableLog_tooLarge`.

Why this is the interesting class: a **declared-but-never-emitted** symbol never enters the decoded weight set, so no Kraft/tableLog bound on the weights can reject it — zstdx previously accepted such headers whenever the emitted weight set was Kraft-valid (proven by round-trip through zstdx's own encoder pipeline with `norm[13]=1`). The Kraft/tablelog argument only covers *emitted* weights (any weight value ≥ 13 forces tableLog ≥ 13 > 12; weight 12 forces tableLog 12 which the max-bits checks kill), not the declared alphabet.

Change: `HuffmanTable::new` builds its weight `FSETable` at `FSETable::new(11)` instead of 255. Over-range declarations now fail at the NCount read with `TooManySymbols` (wrapped as `HuffmanTableError::FSETableError`), the same layer where libzstd rejects.

No-valid-stream-regression proof: every libzstd-accepted weights header passes the workspace check, hence declares maxSV ≤ 11, hence still parses under the cap; the flipped inputs are exactly the maxSV ≥ 12 headers, all of which libzstd rejects. Note the cap also makes weight value 12 unreachable via the FSE form — matching libzstd, where the same workspace check precludes it (the raw 4-bit form still carries weight-12 values, and its existing build-time checks are untouched).

Tests: `weight_fse_alphabet_cap` (declared-never-emitted 11 parses with exact weight round-trip; 12 → `TooManySymbols(13)`), via the new `fse_table_with_declared` helper. R64's `fse_weights_decode_count_bound` (255 legal weights) stays green — its NCount maxSV is 2.
