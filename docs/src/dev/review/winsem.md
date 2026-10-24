# Window semantics

This page records two sibling defects around the declared window on unpledged streams, found during the [optimal parser review](opt.md). The encoder emitted offsets past the declared window, which libzstd rejects, and at w16 its ring decodes wrong bytes with only the checksum catching it, the worst failure class. The decoder was lenient in the other direction, because it validated offsets against available history and accepted frames that libzstd rejects as corruption. The page answers how each half was fixed and how the new semantics were pinned. The encoder fix is `2fca1ef9` and the decoder fix is `09c402fe`.

## Encoder domain clamp

The root cause was that each family's search domain used `params.chain_reach.unwrap_or(params.window)`. `params_for_row` applied a forced `window_log` to `p.window`, but `adjust_params(p, shape.len)` returned early on an unknown length, so `chain_reach` and the table geometry kept their stock ladder values. The LDM size gate disarmed LDM at windows below 1<<25, so the opt bridge's window lift never fired either. Rows 9, 13 to 15 and 16 to 19 scanned up to their stock reach while the header declared the forced window. Row 9 is the chain row with reach 1<<22, rows 13 to 15 are btlazy with reach 1<<22, and rows 16 to 19 are opt with reach 1<<22 or 1<<23.

C avoids this class architecturally. Every search loop is bounded by `ZSTD_getLowestMatchIndex(ms, curr, cParams->windowLog)`, which is `withinWindow = curr - maxDistance` at the final windowLog, user-forced value included, and it applies unconditionally at `zstd_lazy.c:257/2029/2070/2102/2156` and `zstd_opt.c:471/619`. `adjustCParams`' log clamps, `cycleLog > dictAndWindowLog` and `hashLog > dictAndWindowLog+1`, run only under a known srcSize, and they are memory and aliasing clamps. Legality comes from `getLowestMatchIndex`. zstdx's bt ring stores 2 u32 link slots per position, so its `bt_log` is exactly C's cycleLog, where `cycleLog = chainLog - btScale` and btScale is 1 for strat at least btlazy2. `bt_log <= windowLog` therefore mirrors C.

The fix splits `adjust_params` into the length-gated window downsize and an unconditional `clamp_geometry_to_window`. The clamp holds chain_reach, chain_log, row_log and bt_log at or below windowLog, and hash_log and the dfast short table at or below windowLog+1. This moves the legality half of C's semantics to where C carries it, expressed on our domain knobs. The function is idempotent with the known-length path, and it is a no-op on every stock ladder row because reach is at most window by construction. Stock frames therefore keep their bytes, as the dump grids prove.

### Family audit

The audit used a forced window on an unpledged stream with a json 300 KiB prefix and the pre-fix reference decode.

| Rows | Family | Domain carrier | Pre-fix | Post-fix |
|---|---|---|---|---|
| 1-2 | fast | window, reach None | ok | ok, hash clamps to wlog+1 and bytes move about 0.1-0.3%, the bulk path's relation, which gives consistency and not a regression |
| 3-4 | dfast | window, reach None | ok | ok, same note |
| 5-8, 10-12 | row | window, reach None | ok | ok |
| 9 | chain with reach 1<<22 | chain_reach | reject, w10 and w12 corruption, w16 checksum | ok |
| 13-15 | btlazy with reach 1<<22 | chain_reach | reject | ok |
| 16-19 | opt with reach 1<<22 or 1<<23 | chain_reach, LDM lift never armed at forced small windows | reject | ok |
| 20-22 | opt, no reach | window | ok, control | ok |

### Gates

The const ladder proof gained the forced-window axis, with None and 10..=27 on both the length and window axes, per-cell domain assertions, and the collision pin moving from 132 to 1388. `encode_edges::forced_window_unpledged_streams` fails before the fix at the first cell. The dump `--all-levels` and `--stream --all-levels` grids are byte-identical, the ratio geo-mean stays exactly at +8.69% over 120 cells, and the suites, paranoid and the QUICK encode fuzz smoke pass. `dict_seed_key` resolves through the same `params_for_row`, so seeding stays in lockstep.

Size growth on the violating cells after the fix is the fix working, because the encoder loses matches outside the window.

| Cell | Pre-fix size | Post-fix size |
|---|---|---|
| json unpledged stream l9 w10 | 55548 | 59649 |
| json unpledged stream l13 w10 | 55927 | 61017 |
| json unpledged stream l16 w10 | 53989 | 59228 |
| json unpledged stream l19 w10 | 53280 | 58617 |
| json unpledged stream l9 w16 | 43848 | 43731 |
| json unpledged stream l13 w16 | 46170 | 44943 |
| json unpledged stream l16 w16 | 42664 | 42402 |
| json unpledged stream l19 w16 | 41919 | 41866 |

The l19 rows are the no-reach rows, and they are all window-bound now.

## Decoder generation model

libzstd streaming never compares an offset against the declared window. The rule is per-sequence, `RETURN_ERROR_IF(sequence.offset > (size_t)(oLitEnd - virtualStart))`, at 4 sites in `ZSTD_decompressSequences*`. The offset must stay in the current generation. The ring geometry is chosen so the generation reach and the window almost always coincide, but they are not the same predicate. `ZSTD_decodingBufferSize_internal` floors `windowSize` at 1<<10, adds the `+ 2*blockSizeMax + 2*WILDCOPY_OVERLENGTH` slack, and caps by FCS. The ring wraps when `outStart + blockSizeMax > outBuffSize` and the FCS does not cover the buffer. Under libzstd streaming, `fa_w16_o100000` has offset 100000 above the 65536 window but within the 180009-byte generation, and it is accepted, while `sw6_w16_o200000` is rejected once the origin has wrapped.

zstdx now mirrors the model end to end. `FlatOut` tracks ring generations through origin, prev_origin, prev_phys_base and seg_a_end, with libzstd's wrap threshold and FCS no-wrap gate. A raw dictionary's content is a virtual generation-0 prefix that expires at the first wrap, like C's ring discarding the dict extDict. The fused flat executor folds the generation floor into `vbase_op`, so the per-sequence check keeps the whole-history shape in every instantiation. `copy_wrapped_match` un-folds through the `FlatView` it already holds, with a fast linear walk for previous-generation sources and a split doubling walk for straddlers. One-shot, the `All` mode, stays whole-history by design. C's `ZSTD_decompress` decodes into one buffer where `virtualStart == prefixStart == dst`, so any in-history offset is accepted. Matching another C asymmetry, only the one-shot flat path lifts the Raw and RLE `blockSizeMax` cap at execution, while streaming enforces it at the header stage and `ZSTD_decompressFrame` never does. The ring executor's dictionary tail stays reachable only while nothing was drained, and the segment and piece views in `mt.rs` and `mt_pieces.rs` follow the same contract.

The semantics were pinned before the rewrite with a C probe, `probe.c` on libzstd 1.5.x, over crafted frames in the modes oneshot, stream, stream64k, oneshot_dict and stream_dict. The cases covered controls, far offsets, pledged caps, small-window sweeps, 6-block 360 KiB histories and dict-prefill expiries. Streaming flips to reject exactly at the generation reach in the key outcomes. At w10 the cap is 3136, seen in `fa_w10_o100000` and `sw_w10_o3100/3168/3200`. At w16 the wrap is at 180000, seen in `sw6_w16_o200000..360000`, while sizes of 131000 and below are accepted. A declared FCS caps the ring but does not disable wrapping, and the dict reach is dictSize plus produced bytes until the first wrap. `decode_window_semantics.rs` pins every class per decode path. The parity suite gained the `OutWindowInHistory`, `InGenerationPair` and `CrossFrameOffset` crafts, and the full sweep of 45078 cases found 0 hard divergences, with the window-violating one-shot class adjudicated as `oneshot-asymmetry`.

One deliberate deviation is pinned. libzstd's streaming single-pass shortcut, which runs when the FCS is known, the whole frame is in the input and the output room is at least the FCS, accepts out-of-window offsets through `ZSTD_decompress_usingDDict`. For example, `pledged_w10_f60015_o50000` is accepted under stream64k. zstdx streaming always applies the generation model and rejects it. This affects the invalid-frame class only.

## Combined gate on main

On main, the dump `--all-levels` grid of 30 cells and the `--stream --all-levels` grid of 120 cells are byte-identical against the encoder-only tree. The ratio geo-mean is +8.69%, and the worst cell is unchanged. The forced-window interop spot-check is 8/8 reference-accept on json and text at w10 and w16 and at l16 and l19, with sizes matching the encoder half's table. Crafted frames through the streaming CLI match the probe table 5/5. gungraun decode reads flat to negative against the base: stream_decode json -0.09%, oneshot json and text -1.4 to -1.9%, and small cases about 0.006% either way.

## Fidelity deviations fixed

Both encoder deviations left open earlier are closed, and neither touches a stock-ladder byte, since the dump grids and the ratio geo-mean are unchanged.

### Small-source btlazy swap

The small-source btlazy swap at levels 9 and 10 for sources of at most 128 KiB built its own `btlazy(17, 1<<17)` row on the known-length path and never folded `shape.window_log` in, so a forced w10 on a 100 KiB source declared 1<<17. C keeps the forced window. `ZSTD_getCParamsFromCCtxParams` at `zstd_compress.c:1657-1671` picks the small table by srcSizeHint through `tableID = (rSize <= 256 KB) + (rSize <= 128 KB) + (rSize <= 16 KB)` at line 8293. It then applies the user's value through `ZSTD_overrideCParams` with `if (overrides->windowLog) cParams->windowLog = overrides->windowLog;` at line 1648. Then `ZSTD_adjustCParams_internal` runs, and its only window moves are the srcLog down-clamp `if (cPar.windowLog > srcLog) cPar.windowLog = srcLog;` at line 1579 and the ABSOLUTEMIN=10 floor at lines 1589-90. The swap now applies the forced window before its own `adjust_params`, so it resolves like every ladder row, with the source clamp below and `clamp_geometry_to_window` under the final window. `prove_ladder_geometry` already swept the swap cells over the forced-window axis, so the const proof extended for free and the collision pin stays 1388.

### Far-screen lift versus forced window

On an unpledged fast-row stream whose far screen accepts, the lift replaced `params.window` with 1<<26 and the header serialized the widened value, so the screen verdict outranked the user's window. The same class covered the length-driven `Frame` bar at 1<<26 and above and the stream-mt capture re-serialization. In C the forced value is hard. The LDM default lift runs before the override, with `if (CCtxParams->ldmParams.enableLdm == ZSTD_ps_enable) cParams.windowLog = ZSTD_LDM_DEFAULT_WINDOW_LOG;` at line 1666 and `ZSTD_overrideCParams` at line 1667. The CLI's `--long=N` sets ldm plus `windowLog=N` at `zstdcli.c:1147-1163`, and adjustCParams never raises the value.

The fix covers three sites, all in cold parameter resolution. `fast_row_far_class` returns false under `shape.window_log.is_some()`, so the far screens and capture windows stand down exactly as under an explicit LDM switch. `apply_level`'s Auto lift arm requires an AUTO window through `self.shape.window_log.is_none()`, so the length-driven Frame bar follows the same rule, and `dict_seed_key`'s mirror carries the same guard. The frame-header window descriptor no longer floors its exponent at 1, using `(windowLog - ZSTD_WINDOWLOG_ABSOLUTEMIN) << 3` like C, so a forced w10 declares 2^10 exactly. The pre-fix floor declared 2^11, which was the same defect at one power. Auto-window behavior is byte-identical, since the auto twins' 1<<26 headers are pinned in `forced_window_caps_the_far_class_lift`, and the Enable screen skip without a forced window is inert because Enable takes its own arm and its captures were already stood down.

The pins are `small_src_swap_honors_a_forced_window`, a unit test of the swap geometry under w10, and `forced_window_small_src_swap` for bulk and pledged stream with the declared window and reference decode. `forced_window_caps_the_far_class_lift` covers st and mt unpledged far-class streams across auto and forced windows plus the full-bar length hint. `ldm_mode_stream_mt_raised_window_capture_grid`'s Auto cell is re-pinned to the forced 2^25. gungraun was not run, because every change sits in per-frame parameter resolution or header serialization and touches no per-block or per-match code.

## Left open

- The malformed `single_f180015_o100000` probe frame, a hand-rolled 2-byte-FCS single-segment header, is left open. The reference rejects it too, so it is not a semantics datapoint and stays excluded from the pins.
