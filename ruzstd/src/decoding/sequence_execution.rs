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

/// Execute the decoded sequences directly into a flat output slice, starting
/// at `*written`. This is the slice-decode fast path: the frame's already
/// produced bytes live at `out[..*written]` and serve as the match window, so
/// no ring buffer is involved.
///
/// The caller must ensure the block's total output (all literals plus all
/// match lengths) fits into `out[*written..]`; the executor then knows every
/// write stays in bounds and skips per-sequence capacity checks.
pub(crate) fn execute_sequences_flat(
    sequences: &[Sequence],
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    offset_hist: &mut [u32; 3],
) -> Result<(), ExecuteSequencesError> {
    let out_ptr = out.as_mut_ptr();
    let mut w = *written;
    let mut lit_pos = 0usize;

    for &seq in sequences {
        let ll = seq.ll as usize;
        if ll > 0 {
            let high = lit_pos + ll;
            if high > literals.len() {
                return Err(ExecuteSequencesError::NotEnoughBytesForSequence {
                    wanted: high,
                    have: literals.len(),
                });
            }
            // SAFETY: high <= literals.len() by the check above; w + ll stays
            // below out.len() through the caller's per-block budget
            unsafe {
                core::ptr::copy_nonoverlapping(literals.as_ptr().add(lit_pos), out_ptr.add(w), ll);
            }
            lit_pos = high;
            w += ll;
        }

        let actual_offset = do_offset_history(seq.of, seq.ll, offset_hist);
        if actual_offset == 0 {
            return Err(ExecuteSequencesError::ZeroOffset);
        }
        let offset = actual_offset as usize;
        if offset > w {
            // No dictionary in the flat path, so an offset past the frame's
            // own output is always corruption.
            return Err(ExecuteSequencesError::DecodebufferError(
                crate::decoding::errors::DecodeBufferError::OffsetTooBig { offset, buf_len: w },
            ));
        }
        let ml = seq.ml as usize;
        if ml > 0 {
            // Overlapping matches grow in doubling chunks anchored at
            // `w - offset` (same scheme as repeat_in_chunks): after `copied`
            // appended bytes the readable span is `offset + copied` long, so
            // every chunk reads only already-written bytes.
            let mut copied = 0;
            while copied < ml {
                let chunk = (offset + copied).min(ml - copied);
                // SAFETY: src range [w-offset, w-offset+chunk) lies inside the
                // written region; dst end stays below the block budget
                unsafe {
                    core::ptr::copy(out_ptr.add(w - offset), out_ptr.add(w + copied), chunk);
                }
                copied += chunk;
            }
            w += ml;
        }
    }

    let rest = literals.len() - lit_pos;
    if rest > 0 {
        // SAFETY: rest literals fit the block budget like the copies above
        unsafe {
            core::ptr::copy_nonoverlapping(literals.as_ptr().add(lit_pos), out_ptr.add(w), rest);
        }
        w += rest;
    }
    *written = w;
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
