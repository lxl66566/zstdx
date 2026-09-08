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

    while let Some(seq) = dec
        .next()
        .map_err(DecompressBlockError::DecodeSequenceError)?
    {
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
        // SAFETY: high <= literals.len() by the check above; w + ll stays
        // below out_len through the budget check
        unsafe {
            core::ptr::copy_nonoverlapping(literals.as_ptr().add(*lit_pos), out_ptr.add(*w), ll);
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
        if NOWRAP {
            // No wrap has happened: virtual addresses are physical offsets,
            // so the anchor is a plain pointer. Overlapping matches grow in
            // doubling chunks anchored at the match source (same scheme as
            // repeat_in_chunks): after `copied` appended bytes the readable
            // span is `offset + copied` long, so every chunk reads only
            // already-written bytes.
            // SAFETY: cur_v - offset >= 0 was checked above and the doubling
            // cap keeps reads inside the already-written region
            let src = unsafe { base.add(cur_v - offset) };
            let mut copied = 0;
            while copied < ml {
                let chunk = (offset + copied).min(ml - copied);
                // SAFETY: src range [src, src + chunk) lies inside the
                // written region; dst end stays below the budget
                unsafe {
                    core::ptr::copy(src, out_ptr.add(*w + copied), chunk);
                }
                copied += chunk;
            }
        } else {
            // Copy `ml` bytes from the virtual source `cur_v - offset`,
            // splitting at the physical segment boundary. Within a segment
            // the doubling scheme applies: the source anchor stays fixed and
            // each chunk is capped by the already-written span behind the
            // anchor, so reads never run ahead of the write cursor.
            let mut src_v = cur_v - offset;
            let mut dst = *w;
            let mut remaining = ml;
            while remaining > 0 {
                // Map into the active segment, or into the previous segment
                // below its physical end; anything lower is out of window.
                let (src_abs, seg_end) = if src_v >= origin {
                    (src_v - origin, seg_out_len)
                } else if src_v >= prev_origin {
                    (src_v - prev_origin, seg_a_end)
                } else {
                    return Err(ExecuteSequencesError::DecodebufferError(
                        crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                            offset,
                            buf_len: cur_v,
                        },
                    ));
                };
                let chunk = remaining.min(seg_end - src_abs);
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
