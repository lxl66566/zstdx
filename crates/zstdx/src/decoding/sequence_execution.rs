use super::scratch::DecoderScratch;
use crate::blocks::sequence_section::Sequence;
use crate::decoding::errors::ExecuteSequencesError;

/// Largest decompressed block; a single sequence's match can never exceed it
/// (ll + ml fit one block's content).
const MAX_BLOCK_USIZE: usize = crate::common::MAX_BLOCK_SIZE as usize;

/// Take the provided decoder and execute the sequences stored within
pub fn execute_sequences(scratch: &mut DecoderScratch) -> Result<(), ExecuteSequencesError> {
    let old_buffer_size = scratch.buffer.len();

    // Reserve once for the entire block: every literal plus every match
    // length. Each append below consumes exactly its share of this budget,
    // so no per-sequence capacity check is needed (dict-backed appends
    // reserve their own share internally; both sides of the budget shrink
    // in lockstep with them, keeping the invariant).
    let total_out: usize = scratch
        .sequences
        .iter()
        .map(|seq| seq.ml as usize)
        .sum::<usize>()
        .saturating_add(scratch.literals_buffer.len());
    scratch.buffer.reserve(total_out);

    let DecoderScratch {
        sequences,
        literals_buffer,
        buffer,
        offset_hist,
        ..
    } = scratch;

    let mut literals_copy_counter = 0;
    let mut seq_sum = 0;

    for &seq in sequences.iter() {
        if seq.ll > 0 {
            let high = literals_copy_counter + seq.ll as usize;
            if high > literals_buffer.len() {
                return Err(ExecuteSequencesError::NotEnoughBytesForSequence {
                    wanted: high,
                    have: literals_buffer.len(),
                });
            }
            let literals = &literals_buffer[literals_copy_counter..high];
            literals_copy_counter += seq.ll as usize;

            buffer.push_pre_reserved(literals);
        }

        let actual_offset = do_offset_history(seq.of, seq.ll, offset_hist);
        if actual_offset == 0 {
            return Err(ExecuteSequencesError::ZeroOffset);
        }
        if seq.ml > 0 {
            buffer.repeat_pre_reserved(actual_offset as usize, seq.ml as usize)?;
        }

        seq_sum += seq.ml;
        seq_sum += seq.ll;
    }
    if literals_copy_counter < literals_buffer.len() {
        let rest_literals = &literals_buffer[literals_copy_counter..];
        buffer.push_pre_reserved(rest_literals);
        seq_sum += rest_literals.len() as u32;
    }

    let diff = buffer.len() - old_buffer_size;
    assert!(
        seq_sum as usize == diff,
        "Seq_sum: {} is different from the difference in buffersize: {}",
        seq_sum,
        diff
    );
    Ok(())
}

/// Execute the sequences decoded by `dec` straight into the flat target at
/// `out[*written..]`, one sequence at a time — decoding and execution fused,
/// the libzstd model. The frame's already produced bytes at the backing
/// buffer's lower addresses serve as the match window, so no ring buffer is
/// involved.
///
/// `virt_base`/`view` map virtual match addresses to physical offsets in the
/// backing buffer (see `FlatView`): the active segment at the buffer start
/// plus the wrapped-away previous segment that still serves as the window.
/// The never-wrapped case (`view.origin == 0`: all of slice decoding plus a
/// streaming buffer's first generation) runs a monomorphized copy of the loop
/// without the two-generation mapping branches.
///
/// `headroom` says the backing allocation holds at least the block maximum
/// plus wildcopy slack beyond the cursor (the flat streaming/MT buffers
/// guarantee this per block, see `FlatOut::ensure_block_space`), so the
/// monomorphized HEADROOM instantiation wildcopies unconditionally. The
/// per-sequence budget check `op + ll + ml <= out_end` runs in BOTH
/// instantiations: a corrupt sequence section may claim more output than the
/// block maximum, and only that check keeps the cursor inside the allocation
/// (matching libzstd's `op + ll + ml > oend` rejection).
pub(crate) fn execute_decoded_flat(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
    headroom: bool,
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    // BMI2 compiles the decoder's variable shifts to single-uop shlx/shrx;
    // the detection cache makes this dispatch cheap relative to a block.
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if std::is_x86_feature_detected!("bmi2") {
            // SAFETY: bmi2 was just detected at runtime
            return unsafe {
                execute_decoded_flat_bmi2(
                    dec,
                    literals,
                    out,
                    written,
                    virt_base,
                    view,
                    offset_hist,
                    headroom,
                )
            };
        }
    }
    execute_decoded_flat_impl(
        dec,
        literals,
        out,
        written,
        virt_base,
        view,
        offset_hist,
        headroom,
    )
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "bmi2")]
unsafe fn execute_decoded_flat_bmi2(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
    headroom: bool,
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    execute_decoded_flat_impl(
        dec,
        literals,
        out,
        written,
        virt_base,
        view,
        offset_hist,
        headroom,
    )
}

#[inline(always)]
fn execute_decoded_flat_impl(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
    headroom: bool,
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    if view.origin == 0 {
        if headroom {
            execute_decoded_flat_inner::<true, true>(
                dec,
                literals,
                out,
                written,
                virt_base,
                view,
                offset_hist,
            )
        } else {
            execute_decoded_flat_inner::<true, false>(
                dec,
                literals,
                out,
                written,
                virt_base,
                view,
                offset_hist,
            )
        }
    } else if headroom {
        execute_decoded_flat_inner::<false, true>(
            dec,
            literals,
            out,
            written,
            virt_base,
            view,
            offset_hist,
        )
    } else {
        execute_decoded_flat_inner::<false, false>(
            dec,
            literals,
            out,
            written,
            virt_base,
            view,
            offset_hist,
        )
    }
}

#[inline(always)]
fn execute_decoded_flat_inner<const NOWRAP: bool, const HEADROOM: bool>(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    use super::sequence_section_decoder::decode_step;
    use crate::decoding::errors::DecompressBlockError;

    let out_base = out.as_mut_ptr();
    let out_end = out_base.wrapping_add(out.len());
    let lit_base = literals.as_ptr();
    let lit_end = lit_base.wrapping_add(literals.len());
    // `out` is the slice buf[block_start..] of the backing buffer. Within the
    // active segment virtual distances equal physical ones. Two folded bases
    // carry everything the loop needs of the virtual/physical mapping:
    // `vbase_op` is the virtual address of out[0] (the offset bound check is
    // `offset > op + vbase_op`, i.e. `offset > virt_base + pos`), and
    // `wrap_base` is the backing buffer's base (the wrapped-source check is
    // `offset + wrap_base > op`, i.e. `offset > block_start + pos`; unused in
    // the never-wrapped instantiation). The full `view` mapping only feeds
    // the cold wrapped-match path.
    let block_start = if NOWRAP {
        virt_base
    } else {
        virt_base.saturating_sub(view.origin)
    };
    let vbase_op = virt_base.wrapping_sub(out_base as usize);
    let wrap_base = (out_base as usize).wrapping_sub(block_start);

    // The decode state runs as loop locals instead of `dec.next()` calls:
    // fields stored through the `&mut` per iteration pinned the bit window
    // and the three stream states to memory (SROA could not promote them),
    // which showed as a stack round-trip on every sequence. On success the
    // locals are written back for `finish`; an error aborts the frame, so a
    // stale struct is never observed.
    let super::sequence_section_decoder::SeqDecoder {
        tbl,
        src_ptr,
        mut ip,
        mut win,
        mut consumed,
        mut ll_state,
        mut ml_state,
        mut of_state,
        mut rem,
    } = *dec;

    // Output and literal cursors as pointers: the bases fold into them, and
    // `w`/`lit_pos` stop being separate live values.
    let mut op = out_base.wrapping_add(*written);
    let mut lit = lit_base;

    while rem != 0 {
        let seq = decode_step(
            tbl,
            src_ptr,
            &mut ip,
            &mut win,
            &mut consumed,
            &mut ll_state,
            &mut ml_state,
            &mut of_state,
            &mut rem,
        )
        .map_err(DecompressBlockError::DecodeSequenceError)?;
        exec_one_flat::<NOWRAP, HEADROOM>(
            seq,
            &mut op,
            out_base,
            out_end,
            &mut lit,
            lit_base,
            lit_end,
            vbase_op,
            wrap_base,
            view,
            offset_hist,
        )
        .map_err(DecompressBlockError::ExecuteSequencesError)?;
    }

    dec.ip = ip;
    dec.win = win;
    dec.consumed = consumed;
    dec.ll_state = ll_state;
    dec.ml_state = ml_state;
    dec.of_state = of_state;
    dec.rem = rem;

    let rest = lit_end as usize - lit as usize;
    if rest > 0 {
        if op as usize + rest > out_end as usize {
            return Err(DecompressBlockError::ExecuteSequencesError(
                ExecuteSequencesError::TargetTooSmall,
            ));
        }
        // SAFETY: budget checked above; plain copy, no overshoot
        unsafe {
            core::ptr::copy_nonoverlapping(lit, op, rest);
            op = op.add(rest);
        }
    }
    *written = op as usize - out_base as usize;
    dec.finish()
        .map_err(DecompressBlockError::DecodeSequenceError)
}

/// Inline 8-byte copy: variable-length libc calls cost ~15-25 cycles of
/// PLT and prologue each, which dominated the fused loop on sequence-dense
/// payloads (millions of ~5-byte copies per 32 MiB). Fixed 8/16-byte
/// load-store pairs have no such overhead. Both pointers must allow 8
/// readable/writable bytes (the wildcopy budget checks provide them).
#[inline(always)]
unsafe fn copy8(dst: *mut u8, src: *const u8) {
    dst.cast::<u64>()
        .write_unaligned(src.cast::<u64>().read_unaligned());
}

/// Inline 16-byte copy (see [`copy8`]).
#[inline(always)]
unsafe fn copy16(dst: *mut u8, src: *const u8) {
    dst.cast::<u128>()
        .write_unaligned(src.cast::<u128>().read_unaligned());
}

/// libzstd's dec32 table for spreading a sub-8 offset, and the matching
/// net source adjustment `8 - dec64` (entries 5..8 move the source BACK —
/// signed, the usize form of the subtraction underflows).
const DEC32: [usize; 8] = [0, 1, 2, 1, 4, 4, 4, 4];
const DEC_BACK: [isize; 8] = [0, 0, 0, 1, 0, -1, -2, -3];

/// Copy 8 bytes from `*src` to `*dst` so that the source distance afterwards
/// is at least 8, letting the 8-byte chunk loop proceed without reading
/// unwritten bytes (libzstd's `ZSTD_overlapCopy8`). The distance `dst - src`
/// must be in 1..16; both cursors advance by 8. Pointers (not indices) so the
/// distance is computed in one address space regardless of which segments the
/// caller's buffers belong to.
#[inline(always)]
unsafe fn overlap_copy8(dst: &mut *mut u8, src: &mut *const u8) {
    let offset = (*dst as usize).wrapping_sub(*src as usize);
    debug_assert!((1..16).contains(&offset));
    if offset < 8 {
        let d = *dst;
        let s = *src;
        // The first four bytes go one at a time: with offset < 4 each store
        // feeds the next load (overlapping-copy semantics), so a wide load
        // here would read unwritten bytes.
        *d = *s;
        *d.add(1) = *s.add(1);
        *d.add(2) = *s.add(2);
        *d.add(3) = *s.add(3);
        let s2 = s.add(DEC32[offset]);
        // This load only touches bytes at or below d+4 that the stores above
        // (or earlier output) have already written.
        d.add(4)
            .cast::<u32>()
            .write_unaligned(s2.cast::<u32>().read_unaligned());
        *src = s2.offset(DEC_BACK[offset]);
    } else {
        copy8(*dst, *src);
        *src = src.add(8);
    }
    *dst = dst.add(8);
}

/// Wildcopy `ml` bytes from `src0` to `dst0` in 16/8-byte inline chunks,
/// overshooting the end by up to 16 bytes. A distance >= 16 uses 16-byte
/// chunks (the source stays 16 bytes behind the write cursor); below 16 the
/// offset is first spread to >= 8 and 8-byte chunks follow. The caller
/// guarantees the overshoot stays inside the output buffer (budget check
/// `end + 16 <= out_len`); the garbage it leaves beyond the sequence end is
/// always overwritten in address order by the next sequence's literals or
/// match before anything can read it.
#[inline(always)]
unsafe fn wildcopy_match(mut d: *mut u8, mut s: *const u8, ml: usize) {
    let end = d.add(ml);
    if (d as usize).wrapping_sub(s as usize) >= 16 {
        while d < end {
            copy16(d, s);
            d = d.add(16);
            s = s.add(16);
        }
    } else {
        overlap_copy8(&mut d, &mut s);
        while d < end {
            copy8(d, s);
            d = d.add(8);
            s = s.add(8);
        }
    }
}

/// Copy `ll` literals from `*lit` to `*op` with inline 16-byte chunks and
/// the same overshoot contract as [`wildcopy_match`]. Reads may run up to 15
/// bytes past the literals' end, so the caller must keep that inside the
/// literals buffer's allocation (16 bytes reserved).
#[inline(always)]
unsafe fn wildcopy_literals(d0: *mut u8, s0: *const u8, ll: usize) {
    copy16(d0, s0);
    if ll > 16 {
        let end = d0.add(ll);
        let mut d = d0.add(16);
        let mut s = s0.add(16);
        while d < end {
            copy16(d, s);
            d = d.add(16);
            s = s.add(16);
        }
    }
}

/// Execute a single decoded sequence into the flat target: the literal copy,
/// offset-history resolution, and the match copy. `op`/`lit` are the output
/// and literal cursors; `vbase_op`/`wrap_base` are the executor's folded
/// bases (see `execute_decoded_flat_inner`).
///
/// Bounds are one merged budget per sequence: `op + ll + ml` must stay inside
/// the target — corruption can pack more output into a sequence section than
/// the block maximum, and this check is what rejects it (as libzstd does).
/// HEADROOM then only decides the copy strategy: with it, the 16-byte
/// wildcopy overshoot is guaranteed by the backing allocation's slack, so
/// every copy wildcopies unconditionally.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn exec_one_flat<const NOWRAP: bool, const HEADROOM: bool>(
    seq: Sequence,
    op: &mut *mut u8,
    out_base: *mut u8,
    out_end: *mut u8,
    lit: &mut *const u8,
    lit_base: *const u8,
    lit_end: *const u8,
    vbase_op: usize,
    wrap_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
) -> Result<(), crate::decoding::errors::ExecuteSequencesError> {
    use crate::decoding::errors::ExecuteSequencesError;
    let ll = seq.ll as usize;
    let ml = seq.ml as usize;
    let opa = *op as usize;
    let end = opa + ll + ml;
    let enda = out_end as usize;
    if end > enda {
        return Err(ExecuteSequencesError::TargetTooSmall);
    }
    let wild = HEADROOM || end + 16 <= enda;
    if ll > 0 {
        if *lit as usize + ll > lit_end as usize {
            return Err(ExecuteSequencesError::NotEnoughBytesForSequence {
                wanted: (*lit as usize - lit_base as usize) + ll,
                have: lit_end as usize - lit_base as usize,
            });
        }
        // Wildcopy when the 16-byte overshoot stays inside `out` (the
        // literal buffer has 16 reserved bytes behind its length); exact
        // libc copy otherwise (short tail near the frame end).
        // SAFETY: bounds as documented; the overshoot is overwritten in
        // address order by later sequences before it is ever read.
        unsafe {
            if wild {
                wildcopy_literals(*op, *lit, ll);
            } else {
                core::ptr::copy_nonoverlapping(*lit, *op, ll);
            }
            *lit = (*lit).add(ll);
        }
    }

    let actual_offset = do_offset_history(seq.of, seq.ll, offset_hist);
    if actual_offset == 0 {
        return Err(ExecuteSequencesError::ZeroOffset);
    }
    let offset = actual_offset as usize;
    // Virtual position of the match destination: op still points at the
    // sequence start, so the literal length belongs in every address below.
    let dst_a = opa + ll;
    let pos = dst_a - out_base as usize;
    // No dictionary in the flat path, so an offset past the frame's own
    // output is always corruption. Checked even for ml == 0 to reject
    // corrupt frames eagerly.
    let cur_v = dst_a.wrapping_add(vbase_op);
    if offset > cur_v {
        return Err(ExecuteSequencesError::DecodebufferError(
            crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                offset,
                buf_len: cur_v,
            },
        ));
    }
    if ml > 0 {
        // Source inside the active segment: virtual distances are physical
        // distances there, so the source sits exactly `offset` bytes behind
        // the destination and the whole match runs as one linear copy. The
        // never-wrapped instantiation is always this case.
        if !NOWRAP && offset.wrapping_add(wrap_base) > dst_a {
            copy_wrapped_match(
                view,
                vbase_op + out_base as usize,
                out_base,
                pos,
                offset,
                ml,
                wild,
            )?;
        } else {
            // SAFETY: budget checked; the doubling in the exact path and the
            // distance rules in wildcopy_match keep reads behind the cursor
            unsafe {
                let dst = (*op).add(ll);
                let src = dst.sub(offset);
                if wild {
                    wildcopy_match(dst, src, ml);
                } else {
                    let mut copied = 0;
                    while copied < ml {
                        let chunk = (offset + copied).min(ml - copied);
                        core::ptr::copy(src, dst.add(copied), chunk);
                        copied += chunk;
                    }
                }
            }
        }
    }
    // SAFETY: the merged budget check above proved ll + ml stays inside the
    // target slice
    unsafe { *op = (*op).add(ll + ml) };
    Ok(())
}

/// Match copy for a source at or below the active segment (see `FlatView`):
/// the common case resolves inline at the top as one linear copy (see the
/// fast path below); sources crossing the segment boundary fall to the
/// generic walk, split at the physical edges with doubling out of already
/// written bytes. Out of line — the segment-mapping state is loop-invariant
/// but keeping it live in the fused loop costs every sequence register
/// pressure, and the straddling path is rare outside long-offset-heavy
/// frames.
#[cold]
fn copy_wrapped_match(
    view: crate::decoding::flat_buffer::FlatView,
    virt_base: usize,
    out_ptr: *mut u8,
    pos: usize,
    offset: usize,
    ml: usize,
    wild: bool,
) -> Result<(), crate::decoding::errors::ExecuteSequencesError> {
    use crate::decoding::errors::{DecodeBufferError, ExecuteSequencesError};
    let crate::decoding::flat_buffer::FlatView {
        origin,
        prev_origin,
        seg_a_end,
        out_len: seg_out_len,
    } = view;
    let block_start = virt_base.saturating_sub(origin);
    // Fast path: a source entirely inside the previous segment is still one
    // linear in-buffer copy. The previous segment physically sits
    // `seg_a_end - offset` ABOVE the destination — the wrap margin keeps
    // every in-window source at least MAX_BLOCK_SIZE above the write cursor
    // (so the copy never overlaps and a sequence's ml can never exceed the
    // distance), and its bytes at least 16 under the physical buffer end
    // (so wildcopy overshoot stays in the allocation). `d` is how far the
    // source reaches below the segment start: d >= ml means no straddle.
    // Offsets past the guaranteed window (only possible beyond the frame's
    // window bound) fall through to the generic walk below, which rejects
    // out-of-window sources.
    let d = offset - block_start - pos;
    if d >= ml && offset + MAX_BLOCK_USIZE <= seg_a_end {
        // SAFETY: bounds as documented above
        unsafe {
            let dst = out_ptr.add(pos);
            let src = dst.add(seg_a_end - offset);
            if wild {
                wildcopy_match(dst, src, ml);
            } else {
                core::ptr::copy_nonoverlapping(src, dst, ml);
            }
        }
        return Ok(());
    }
    // SAFETY: out is the slice starting block_start bytes into the buffer
    let base = unsafe { out_ptr.sub(block_start) };
    let mut src_v = virt_base + pos - offset;
    let mut dst = pos;
    let mut remaining = ml;
    while remaining > 0 {
        // Map into the active segment, or into the previous segment below
        // its physical end; anything lower is out of window.
        let (src_abs, seg_end, active) = if src_v >= origin {
            (src_v - origin, seg_out_len, true)
        } else if src_v >= prev_origin {
            (src_v - prev_origin, seg_a_end, false)
        } else {
            return Err(ExecuteSequencesError::DecodebufferError(
                DecodeBufferError::OffsetTooBig {
                    offset,
                    buf_len: virt_base + pos,
                },
            ));
        };
        let chunk = remaining.min(seg_end - src_abs);
        if active && wild {
            // The whole rest of the match lies in the active segment
            // (virtual addresses up to the destination), so finish it in
            // one wildcopy.
            // SAFETY: same contract as the fused loop's wildcopy
            unsafe {
                wildcopy_match(out_ptr.add(dst), base.add(src_abs), remaining);
            }
            break;
        }
        // Already-written bytes behind the anchor, as a virtual distance
        // (positive: the source is always behind dst).
        let readable = virt_base + dst - src_v;
        let mut copied = 0;
        while copied < chunk {
            let c = (readable + copied).min(chunk - copied);
            // SAFETY: src range [src_abs, src_abs + c) ends at or before the
            // write cursor (c <= readable + copied); dst end stays below the
            // block budget
            unsafe {
                core::ptr::copy(base.add(src_abs), out_ptr.add(dst + copied), c);
            }
            copied += c;
        }
        src_v += chunk;
        dst += chunk;
        remaining -= chunk;
    }
    Ok(())
}

/// Update the most recently used offsets to reflect the provided offset value, and return the
/// "actual" offset needed because offsets are not stored in a raw way, some transformations are needed
/// before you get a functional number.
///
/// The repcode domain (`offset_value <= 3`) is handled ZSTD_decodeSequence
/// style: the code plus the literal-length-0 flag forms a slot index
/// (`code - 1 + ll0`), where slot 3 — code 3 with no literals — is the
/// `rep0 - 1` pseudo-slot, folded onto `scratch[0]` with a conditional
/// subtract after the shared load. The same slot picks the history
/// rotation: 0 keeps everything (code 1 with literals), anything else —
/// including real offsets, whose slot is always >= 3 — rotates all three.
///
/// Entirely select-based, no data-dependent branches: repcodes flip too
/// often on structured input for the `<= 3` test and the rotation match to
/// predict, and both sat among the largest branch-miss clusters of the
/// fused decode loop. The slot load stays in bounds for non-repcodes by
/// masking the (garbage) index, whose loaded value the selects then
/// discard.
pub(crate) fn do_offset_history(offset_value: u32, lit_len: u32, scratch: &mut [u32; 3]) -> u32 {
    let idx = offset_value
        .wrapping_sub(1)
        .wrapping_add((lit_len == 0) as u32);
    // idx in 0..=3 exactly for repcodes; (idx & 3) with 3 folded back to 0
    // is the pseudo-slot mapping
    let slot = (idx & 3) as usize;
    let slot = if slot == 3 { 0 } else { slot };
    let actual_offset = if offset_value <= 3 {
        // A malformed dictionary can seed scratch[0] with 0; saturate so this
        // resolves to 0 (rejected upstream as ZeroOffset) instead of
        // underflowing. See #115.
        scratch[slot].saturating_sub((idx == 3) as u32)
    } else {
        offset_value.wrapping_sub(3)
    };

    // Rotation as selects: slot 0 keeps everything, slot 1 swaps the two
    // most recent codes (slot 2 untouched), anything else — including real
    // offsets, whose slot is always >= 3 — rotates all three. Ordered so
    // every read sees the pre-update values.
    let keep = idx == 0;
    let rotate_all = idx >= 2;
    scratch[2] = if rotate_all { scratch[1] } else { scratch[2] };
    scratch[1] = if keep { scratch[1] } else { scratch[0] };
    scratch[0] = if keep { scratch[0] } else { actual_offset };

    actual_offset
}

#[cfg(test)]
mod tests {
    use super::do_offset_history;

    #[test]
    fn repeat_offset_minus_one_with_zero_history_does_not_underflow() {
        // A malformed dictionary can seed offset history slot 0 with 0. With
        // literal length 0 and offset code 3 ("repeat the most recent offset,
        // minus one"), `scratch[0] - 1` must not underflow; it should resolve to
        // 0, which the caller rejects as ExecuteSequencesError::ZeroOffset rather
        // than panicking (debug) or wrapping to u32::MAX (release). See #115.
        let mut scratch = [0u32, 4, 8];
        assert_eq!(do_offset_history(3, 0, &mut scratch), 0);
    }

    /// Reference: the spec's offset-code table, as the match-tree
    /// implementation spelled it before the branch-free rewrite.
    fn reference(of: u32, ll: u32, s: &mut [u32; 3]) -> u32 {
        let actual = if ll > 0 {
            match of {
                1..=3 => s[of as usize - 1],
                _ => of - 3,
            }
        } else {
            match of {
                1..=2 => s[of as usize],
                3 => s[0].saturating_sub(1),
                _ => of - 3,
            }
        };
        match (ll > 0, of) {
            (true, 1) => {}
            (true, 2) | (false, 1) => {
                s[1] = s[0];
                s[0] = actual;
            }
            _ => {
                s[2] = s[1];
                s[1] = s[0];
                s[0] = actual;
            }
        }
        actual
    }

    #[test]
    fn branch_free_history_matches_reference() {
        for ll in [0u32, 1, 7] {
            for of in 1u32..300 {
                for seed in 0..8u32 {
                    let mut a = [seed + 1, seed * 3 + 4, seed * 5 + 8];
                    let mut b = a;
                    let ra = do_offset_history(of, ll, &mut a);
                    let rb = reference(of, ll, &mut b);
                    assert_eq!((ra, a), (rb, b), "of={of} ll={ll} seed={seed}");
                }
            }
        }
        // The zero-seeded pseudo-slot must saturate identically (see #115).
        for of in 1u32..6 {
            let mut a = [0u32, 4, 8];
            let mut b = a;
            assert_eq!(
                do_offset_history(of, 0, &mut a),
                reference(of, 0, &mut b),
                "of={of}"
            );
            assert_eq!(a, b, "of={of}");
        }
    }
}
