# Divergences from libzstd
## Header reserved bits are strict


The FHD reserved bit at bit 3 is rejected at header parse with `FrameDescriptorError::ReservedBitSet`, matching libzstd's "unsupported frame parameter" and its `fhdByte & 0x08` check. The spec's Unused_bit at bit 4 stays accepted, because a compliant decoder shall not interpret it and libzstd ignores it too. The old "bits 4-3" framing was imprecise.

The interop gate covers 1300 reference frames from the fuzz interop, decode and decode_dict payloads crossed with the zstd CLI at -1, -3, -9 and -19, all of which still decode, and the raw decode-corpus pass set is unchanged at 20 files before and after. The two b5593b58 replay tests flipped to reference-parity errors.
## Frame content size is a contract


zstdx once decoded frames without comparing produced output against the header's FCS, so a frame declaring 57 bytes whose blocks produced 5 decoded successfully on every path. Inside a larger stream it gave the misleading downstream error "Reserved block occured", while libzstd reports a size error at the frame tail.

Every frame-finish point now verifies its output count against the declared size and reports `FrameDecoderError::ContentSizeMismatch`, and frames without an FCS stay unchecked. The ring path's count came from `DecodeBuffer`'s `total_output_counter`, which missed raw blocks, RLE blocks and full-dictionary repeats. A per-path counter is only trustworthy once a consumer forces every writer to feed it.

The MT executor never sees a frame tail, so `scan` records every frame's pledge in `ScanPlan::frame_pledges`, and both executors verify each frame's executed span after execution. The piece winner path is exact by construction, because its pieces tile the pledge and a mismatched pledge kills every grid before the fallback. The pledge check runs before the checksum comparison to preserve the sequential paths' error precedence.

The count also has to survive a mid-frame path switch: a `force_dict` takeover moves the frame from the flat counter to the ring counter, and the ring starts at zero. Without recording the flat bytes as a baseline at the takeover, an intact pledged frame was rejected with the full pledge missing exactly the pre-takeover output.
## Streaming generation shortcut


With a known FCS, the whole frame already in the input and enough output room, libzstd's streaming call becomes a one-shot pass through `zdss_loadHeader` and `ZSTD_decompress_usingDDict`, and it accepts out-of-window offsets. `pledged_w10_f60015_o50000` accepts under a 64 KiB-out streaming call and rejects under a 4 KiB-out call.

zstdx streaming always applies the generation model and rejects. The divergence covers invalid frames only and is pinned in `decode_window_semantics::window_semantics_pledged`.
