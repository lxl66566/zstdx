use super::scratch::DecoderScratch;
use crate::blocks::sequence_section::Sequence;
use crate::decoding::errors::ExecuteSequencesError;

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
/// Bounds are checked per sequence (`written + ll + ml <= out.len()`), which
/// replaces the two-pass path's whole-block budget precheck. Commits
/// `*written` and validates the bitstream padding (`dec.finish`) on success.
pub(crate) fn execute_decoded_flat(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    // BMI2 compiles the decoder's variable shifts to single-uop shlx/shrx;
    // the detection cache makes this dispatch cheap relative to a block.
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if std::is_x86_feature_detected!("bmi2") {
            // SAFETY: bmi2 was just detected at runtime
            return unsafe {
                execute_decoded_flat_bmi2(dec, literals, out, written, virt_base, view, offset_hist)
            };
        }
    }
    execute_decoded_flat_impl(dec, literals, out, written, virt_base, view, offset_hist)
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
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    execute_decoded_flat_impl(dec, literals, out, written, virt_base, view, offset_hist)
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
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    if view.origin == 0 {
        execute_decoded_flat_inner::<true>(
            dec,
            literals,
            out,
            written,
            virt_base,
            view,
            offset_hist,
        )
    } else {
        execute_decoded_flat_inner::<false>(
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
fn execute_decoded_flat_inner<const NOWRAP: bool>(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    use crate::decoding::errors::{DecompressBlockError, ExecuteSequencesError};
    use super::sequence_section_decoder::decode_step;

    let out_ptr = out.as_mut_ptr();
    let out_len = out.len();
    // `out` is the slice buf[block_start..] of the backing buffer; recover
    // the buffer base so match sources (absolute offsets) address correctly.
    // With a slice-decode target (virt_base == origin) this is `out` itself.
    let block_start = if NOWRAP {
        virt_base
    } else {
        virt_base.saturating_sub(view.origin)
    };
    // SAFETY: subtracting within the same allocation; the caller guarantees
    // out is the slice starting block_start bytes into the buffer
    let base = unsafe { out_ptr.sub(block_start) };
    let crate::decoding::flat_buffer::FlatView {
        origin,
        prev_origin,
        seg_a_end,
        out_len: seg_out_len,
    } = view;
    let mut w = *written;
    let mut lit_pos = 0usize;

    // The decode state runs as loop locals instead of `dec.next()` calls:
    // fields stored through the `&mut` per iteration pinned the bit window
    // and the three table entries to memory (SROA could not promote them),
    // which showed as a stack round-trip on every sequence. On success the
    // locals are written back for `finish`; an error aborts the frame, so a
    // stale struct is never observed.
    let super::sequence_section_decoder::SeqDecoder {
        tbl,
        src_ptr,
        src_len,
        mut ip,
        mut bits,
        mut win,
        mut consumed,
        mut ll_entry,
        mut ml_entry,
        mut of_entry,
        nseq,
        mut idx,
    } = *dec;

    while idx != nseq {
        let seq = match decode_step(
            tbl, src_ptr, src_len, &mut ip, &mut bits, &mut win, &mut consumed, &mut ll_entry,
            &mut ml_entry, &mut of_entry, nseq, &mut idx,
        ) {
            Ok(Some(seq)) => seq,
            Ok(None) => break,
            Err(e) => return Err(DecompressBlockError::DecodeSequenceError(e)),
        };
        exec_one_flat::<NOWRAP>(
            seq,
            literals,
            &mut lit_pos,
            out_ptr,
            out_len,
            &mut w,
            base,
            virt_base,
            origin,
            prev_origin,
            seg_a_end,
            seg_out_len,
            offset_hist,
        )
        .map_err(DecompressBlockError::ExecuteSequencesError)?;
    }

    dec.ip = ip;
    dec.bits = bits;
    dec.win = win;
    dec.consumed = consumed;
    dec.ll_entry = ll_entry;
    dec.ml_entry = ml_entry;
    dec.of_entry = of_entry;
    dec.idx = idx;

    let rest = literals.len() - lit_pos;
    if rest > 0 {
        if w + rest > out_len {
            return Err(DecompressBlockError::ExecuteSequencesError(
                ExecuteSequencesError::TargetTooSmall,
            ));
        }
        // SAFETY: budget checked above; rest literals fit like the copies above
        unsafe {
            core::ptr::copy_nonoverlapping(literals.as_ptr().add(lit_pos), out_ptr.add(w), rest);
        }
        w += rest;
    }
    *written = w;
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
    dst.cast::<u64>().write_unaligned(src.cast::<u64>().read_unaligned());
}

/// Inline 16-byte copy (see [`copy8`]).
#[inline(always)]
unsafe fn copy16(dst: *mut u8, src: *const u8) {
    dst.cast::<u128>()
        .write_unaligned(src.cast::<u128>().read_unaligned());
}

/// libzstd's dec32/dec64 tables for spreading a sub-8 offset.
const DEC32: [usize; 8] = [0, 1, 2, 1, 4, 4, 4, 4];
const DEC64: [usize; 8] = [8, 8, 8, 7, 8, 9, 10, 11];

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
        *src = s2.add(8 - DEC64[offset]);
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

/// Copy `ll` literals from `literals[lit_pos..]` to `out[w..]` with inline
/// 16-byte chunks and the same overshoot contract as [`wildcopy_match`].
/// Reads may run up to 15 bytes past the literals' end, so the caller must
/// keep that inside the literals buffer's allocation (16 bytes reserved).
#[inline(always)]
unsafe fn wildcopy_literals(
    out: *mut u8,
    literals: *const u8,
    w: usize,
    lit_pos: usize,
    ll: usize,
) {
    copy16(out.add(w), literals.add(lit_pos));
    if ll > 16 {
        let end = w + ll;
        let mut d = w + 16;
        let mut s = lit_pos + 16;
        while d < end {
            copy16(out.add(d), literals.add(s));
            d += 16;
            s += 16;
        }
    }
}

/// Execute a single decoded sequence into the flat target: the literal copy,
/// offset-history resolution, and the match copy.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn exec_one_flat<const NOWRAP: bool>(
    seq: Sequence,
    literals: &[u8],
    lit_pos: &mut usize,
    out_ptr: *mut u8,
    out_len: usize,
    w: &mut usize,
    base: *mut u8,
    virt_base: usize,
    origin: usize,
    prev_origin: usize,
    seg_a_end: usize,
    seg_out_len: usize,
    offset_hist: &mut [u32; 3],
) -> Result<(), crate::decoding::errors::ExecuteSequencesError> {
    use crate::decoding::errors::ExecuteSequencesError;
    let ll = seq.ll as usize;
    let ml = seq.ml as usize;
    if *w + ll + ml > out_len {
        return Err(ExecuteSequencesError::TargetTooSmall);
    }
    if ll > 0 {
        let high = *lit_pos + ll;
        if high > literals.len() {
            return Err(ExecuteSequencesError::NotEnoughBytesForSequence {
                wanted: high,
                have: literals.len(),
            });
        }
        // Wildcopy when the 16-byte overshoot stays inside `out` (the
        // literal buffer has 16 reserved bytes behind its length); exact
        // libc copy otherwise (short tail near the frame end).
        // SAFETY: bounds as documented; the overshoot is overwritten in
        // address order by later sequences before it is ever read.
        unsafe {
            if *w + ll + 16 <= out_len {
                wildcopy_literals(out_ptr, literals.as_ptr(), *w, *lit_pos, ll);
            } else {
                core::ptr::copy_nonoverlapping(
                    literals.as_ptr().add(*lit_pos),
                    out_ptr.add(*w),
                    ll,
                );
            }
        }
        *lit_pos = high;
        *w += ll;
    }

    let actual_offset = do_offset_history(seq.of, seq.ll, offset_hist);
    if actual_offset == 0 {
        return Err(ExecuteSequencesError::ZeroOffset);
    }
    let offset = actual_offset as usize;
    let cur_v = virt_base + *w;
    if offset > cur_v {
        // No dictionary in the flat path, so an offset past the frame's
        // own output is always corruption.
        return Err(ExecuteSequencesError::DecodebufferError(
            crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                offset,
                buf_len: cur_v,
            },
        ));
    }
    if ml > 0 {
        // Wildcopy budget: the 16-byte overshoot must stay inside `out`.
        let wild = *w + ml + 16 <= out_len;
        if NOWRAP {
            // No wrap has happened: virtual addresses are physical offsets,
            // so source and destination address through one linear space.
            // SAFETY: budget checked; the doubling in the exact path and the
            // distance rules in wildcopy_match keep reads behind the cursor
            unsafe {
                let src = base.add(cur_v - offset);
                if wild {
                    wildcopy_match(out_ptr.add(*w), src, ml);
                } else {
                    let mut copied = 0;
                    while copied < ml {
                        let chunk = (offset + copied).min(ml - copied);
                        core::ptr::copy(src, out_ptr.add(*w + copied), chunk);
                        copied += chunk;
                    }
                }
            }
        } else {
            // Copy `ml` bytes from the virtual source `cur_v - offset`,
            // splitting at the physical segment boundary. Sources inside the
            // active segment run as one contiguous wildcopy (the segment is
            // linear and the source stays behind the write cursor); sources
            // in the wrapped-away previous segment, or runs crossing the
            // boundary, keep the exact doubling path.
            let mut src_v = cur_v - offset;
            let mut dst = *w;
            let mut remaining = ml;
            while remaining > 0 {
                // Map into the active segment, or into the previous segment
                // below its physical end; anything lower is out of window.
                let (src_abs, seg_end, active) = if src_v >= origin {
                    (src_v - origin, seg_out_len, true)
                } else if src_v >= prev_origin {
                    (src_v - prev_origin, seg_a_end, false)
                } else {
                    return Err(ExecuteSequencesError::DecodebufferError(
                        crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                            offset,
                            buf_len: cur_v,
                        },
                    ));
                };
                let chunk = remaining.min(seg_end - src_abs);
                if active && wild {
                    // The whole rest of the match lies in the active segment
                    // (virtual addresses up to the destination), so finish it
                    // in one wildcopy.
                    // SAFETY: same contract as the NOWRAP branch
                    unsafe {
                        wildcopy_match(out_ptr.add(dst), base.add(src_abs), remaining);
                    }
                    break;
                }
                // Already-written bytes behind the anchor, as a virtual
                // distance (positive: the source is always behind dst).
                let readable = virt_base + dst - src_v;
                let mut copied = 0;
                while copied < chunk {
                    let c = (readable + copied).min(chunk - copied);
                    // SAFETY: src range [src_abs, src_abs + c) ends at or
                    // before the write cursor (c <= readable + copied); dst
                    // end stays below the block budget
                    unsafe {
                        core::ptr::copy(base.add(src_abs), out_ptr.add(dst + copied), c);
                    }
                    copied += c;
                }
                src_v += chunk;
                dst += chunk;
                remaining -= chunk;
            }
        }
        *w += ml;
    }
    Ok(())
}

/// Update the most recently used offsets to reflect the provided offset value, and return the
/// "actual" offset needed because offsets are not stored in a raw way, some transformations are needed
/// before you get a functional number.
pub(crate) fn do_offset_history(offset_value: u32, lit_len: u32, scratch: &mut [u32; 3]) -> u32 {
    let actual_offset = if lit_len > 0 {
        match offset_value {
            1..=3 => scratch[offset_value as usize - 1],
            _ => {
                //new offset
                offset_value - 3
            }
        }
    } else {
        match offset_value {
            1..=2 => scratch[offset_value as usize],
            // A malformed dictionary can seed scratch[0] with 0; saturate so this
            // resolves to 0 (rejected upstream as ZeroOffset) instead of
            // underflowing. See #115.
            3 => scratch[0].saturating_sub(1),
            _ => {
                //new offset
                offset_value - 3
            }
        }
    };

    //update history
    if lit_len > 0 {
        match offset_value {
            1 => {
                //nothing
            }
            2 => {
                scratch[1] = scratch[0];
                scratch[0] = actual_offset;
            }
            _ => {
                scratch[2] = scratch[1];
                scratch[1] = scratch[0];
                scratch[0] = actual_offset;
            }
        }
    } else {
        match offset_value {
            1 => {
                scratch[1] = scratch[0];
                scratch[0] = actual_offset;
            }
            2 => {
                scratch[2] = scratch[1];
                scratch[1] = scratch[0];
                scratch[0] = actual_offset;
            }
            _ => {
                scratch[2] = scratch[1];
                scratch[1] = scratch[0];
                scratch[0] = actual_offset;
            }
        }
    }

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
}
