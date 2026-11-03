use core::num::NonZero;

use super::scratch::DecoderScratch;
use crate::decoding::errors::ExecuteSequencesError;

/// Proof that a `DecodeBuffer`'s ring buffer was reserved for a whole
/// block's output (Σ literal plus match lengths, minted by
/// [`execute_sequences`]' reserve). The pre-reserved append entries
/// (`DecodeBuffer::push_pre_reserved` / `repeat_pre_reserved`) take it
/// instead of trusting that the block-level reserve covers the append:
/// the typestate replaces the trust, and only the reserve site can
/// construct one (private field).
#[derive(Debug, Clone, Copy)]
pub struct BlockBudget {
    _reserved: (),
}

/// Take the provided decoder and execute the sequences stored within,
/// returning the [`BlockBudget`] its reserve minted.
pub fn execute_sequences(
    scratch: &mut DecoderScratch,
) -> Result<BlockBudget, ExecuteSequencesError> {
    let old_buffer_size = scratch.buffer.len();

    // Reserve once for the entire block: every literal plus every match
    // length. Each append below consumes exactly its share of this budget,
    // so no per-sequence capacity check is needed (dict-backed appends
    // reserve their own share internally; both sides of the budget shrink
    // in lockstep with them, keeping the invariant).
    // Saturating: 32-bit targets must not wrap the untrusted Σml (a hostile
    // block can claim ~2^33 match bytes); a wrapped sum would slip past the
    // block cap below. On 64-bit the sum cannot reach usize::MAX.
    let total_out: usize = scratch
        .sequences
        .iter()
        .map(|seq| seq.ml as usize)
        .fold(0usize, usize::saturating_add)
        .saturating_add(scratch.literals_buffer.len());
    // A block's output is capped by Block_Maximum_Size = min(window, 128 KiB).
    // Enforce the cap before the reserve: a hostile sequence section claiming
    // gigabytes of match output is rejected here instead of being sized for
    // (libzstd rejects such blocks at blockSizeMax before execution).
    let block_out_max = crate::common::max_block_output(scratch.buffer.window_size);
    if total_out > block_out_max {
        return Err(ExecuteSequencesError::BlockOutputTooLarge { max: block_out_max });
    }
    scratch.buffer.reserve(total_out);
    // Minted only after the reserve above: it is the proof every
    // pre-reserved append below relies on.
    let budget = BlockBudget { _reserved: () };

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

            buffer.push_pre_reserved(literals, budget);
        }

        // The resolution owns the zero-offset boundary: a wire value past
        // the +3 bias resolves provably nonzero, and a repcode resolving to
        // zero is the malformed-history bug class rejected here.
        let Some(actual_offset) = do_offset_history(seq.of, seq.ll, offset_hist) else {
            return Err(ExecuteSequencesError::ZeroOffset);
        };
        if seq.ml > 0 {
            buffer.repeat_pre_reserved(actual_offset.get() as usize, seq.ml as usize, budget)?;
        }

        seq_sum += seq.ml;
        seq_sum += seq.ll;
    }
    if literals_copy_counter < literals_buffer.len() {
        let rest_literals = &literals_buffer[literals_copy_counter..];
        buffer.push_pre_reserved(rest_literals, budget);
        seq_sum += rest_literals.len() as u32;
    }

    let diff = buffer.len() - old_buffer_size;
    assert!(
        seq_sum as usize == diff,
        "Seq_sum: {seq_sum} is different from the difference in buffersize: {diff}"
    );
    Ok(budget)
}

/// Execute the sequences decoded by `dec` straight into the flat target at
/// `out[*written..]`, one sequence at a time. The loop ports libzstd's
/// `ZSTD_decompressSequences_body` shape: per-field entry loads, repcodes
/// resolved while decoding, one combined budget check that sends every edge
/// case to an out-of-line exact path, and an unconditional 16-byte literal
/// copy. The frame's already produced bytes at the backing buffer's lower
/// addresses serve as the match window, so no ring buffer is involved.
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
/// HEADROOM instantiation wildcopies up to the target end. Without it the
/// hot path stops 16 bytes short of the end. The budget check runs in both
/// instantiations: a corrupt sequence section may claim more output than the
/// block maximum, and only that check keeps the cursor inside the allocation
/// (libzstd's `op + ll + ml > oend` rejection).
pub(crate) fn execute_decoded_flat(
    dec: &mut super::sequence_section_decoder::SeqDecoder,
    literals: &[u8],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: &crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
    headroom: bool,
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    // BMI2 compiles the decoder's variable shifts to single-uop shlx/shrx;
    // the detection cache makes this dispatch cheap relative to a block.
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if super::simd_tier::bmi2() {
            // SAFETY: simd_tier::bmi2() ran the runtime detection (or a forced
            // tier asserted it), so the BMI2 instantiation is executable
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
    view: &crate::decoding::flat_buffer::FlatView,
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
    view: &crate::decoding::flat_buffer::FlatView,
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
    view: &crate::decoding::flat_buffer::FlatView,
    offset_hist: &mut [u32; 3],
) -> Result<(), crate::decoding::errors::DecompressBlockError> {
    use super::sequence_section_decoder::{EntryRef, LL_SLOT, ML_SLOT, OF_SLOT, Reload, reload};
    use crate::decoding::errors::{DecodeSequenceError, DecompressBlockError};

    let out_base = out.as_mut_ptr();
    let out_end = out_base as usize + out.len();
    // Highest sequence end the hot path may reach: its 16-byte wildcopy
    // overshoot must land inside the target, or inside the backing slack
    // HEADROOM guarantees.
    let oend_w = if HEADROOM {
        out_end
    } else {
        out_end.saturating_sub(16)
    };
    let lit_base = literals.as_ptr() as usize;
    let lit_end = lit_base + literals.len();
    let block_start = if NOWRAP {
        virt_base
    } else {
        virt_base.saturating_sub(view.origin)
    };
    // The previous generation's virtual start, 0 while the buffer never
    // wrapped.
    let floor = if NOWRAP {
        0
    } else {
        view.prev_origin
    };
    // Physical start of the active generation: a source below it sits in
    // the wrapped-away previous generation.
    let prefix_start = (out_base as usize).wrapping_sub(block_start);
    // `virt_base` with the generation floor folded in (see
    // `copy_wrapped_match`).
    let folded_base = virt_base.wrapping_sub(floor);
    // The generation floor in the active generation's address frame: a
    // source below it is out of the window.
    let virtual_start = (out_base as usize).wrapping_sub(folded_base);

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
    // The repcode history lives in locals for the block and is written back
    // on success; an error aborts the frame.
    let mut rep0 = offset_hist[0] as usize;
    let mut rep1 = offset_hist[1] as usize;
    let mut rep2 = offset_hist[2] as usize;
    let mut op = out_base as usize + *written;
    let mut lit = lit_base;

    while rem != 0 {
        rem -= 1;
        // SAFETY: FSE table construction keeps every reachable state below
        // its table size, RLE streams included (see `decode_step`).
        let (ll_e, ml_e, of_e) = unsafe {
            (
                EntryRef::at(tbl, LL_SLOT, ll_state),
                EntryRef::at(tbl, ML_SLOT, ml_state),
                EntryRef::at(tbl, OF_SLOT, of_state),
            )
        };
        let mut ml = ml_e.base() as usize;
        let mut ll = ll_e.base() as usize;
        let of_base = of_e.base() as usize;
        let ll_bits = ll_e.add_bits();
        let ml_bits = ml_e.add_bits();
        let of_bits = of_e.add_bits();
        #[cfg(feature = "seq_dump")]
        let mut of_wire = of_base as u32;

        let offset;
        if of_bits > 1 {
            // Offset codes 2 and up are real offsets: the wire value carries
            // the +3 repcode bias.
            let bits = read_fast(&mut win, &mut consumed, of_bits);
            #[cfg(feature = "seq_dump")]
            {
                of_wire += bits as u32;
            }
            offset = of_base + bits - 3;
            rep2 = rep1;
            rep1 = rep0;
            rep0 = offset;
        } else {
            // Repcodes: code 0 is wire value 1, code 1 reads one bit for
            // wire value 2 or 3. A zero literal length shifts the index by
            // one, and index 3 means rep0 - 1.
            let ll0 = usize::from(ll == 0);
            if of_bits == 0 {
                offset = if ll0 != 0 {
                    rep1
                } else {
                    rep0
                };
                rep1 = if ll0 != 0 {
                    rep0
                } else {
                    rep1
                };
                rep0 = offset;
            } else {
                let bit = read_fast(&mut win, &mut consumed, 1);
                #[cfg(feature = "seq_dump")]
                {
                    of_wire += bit as u32;
                }
                let idx = of_base - 1 + ll0 + bit;
                let temp = match idx {
                    1 => rep1,
                    2 => rep2,
                    // A malformed dictionary can seed rep0 with 0; saturate
                    // so this lands on the ZeroOffset rejection below.
                    _ => rep0.saturating_sub(1),
                };
                if idx != 1 {
                    rep2 = rep1;
                }
                rep1 = rep0;
                rep0 = temp;
                offset = temp;
            }
            if offset == 0 {
                return Err(DecompressBlockError::ExecuteSequencesError(
                    ExecuteSequencesError::ZeroOffset,
                ));
            }
        }
        if ml_bits > 0 {
            ml += read_fast(&mut win, &mut consumed, ml_bits);
        }
        // Up to 31 + 16 bits are read so far, inside the 57 a reload leaves.
        // Wide totals reload here so the ll field and the 26 transition bits
        // fit as well.
        if ll_bits + ml_bits + of_bits >= 31
            && matches!(
                reload(src_ptr, &mut ip, &mut win, &mut consumed),
                Reload::Overflow
            )
        {
            return Err(DecompressBlockError::DecodeSequenceError(
                DecodeSequenceError::NotEnoughBytesForNumSequences,
            ));
        }
        if ll_bits > 0 {
            ll += read_fast(&mut win, &mut consumed, ll_bits);
        }
        #[cfg(feature = "seq_dump")]
        super::seq_dump::record(ll as u32, ml as u32, of_wire);
        // The last sequence has no transitions: they and the reload would
        // read past the stream's end.
        if rem != 0 {
            ll_state = ll_e.next_state() + read_bits(&mut win, &mut consumed, ll_e.nb_bits());
            ml_state = ml_e.next_state() + read_bits(&mut win, &mut consumed, ml_e.nb_bits());
            of_state = of_e.next_state() + read_bits(&mut win, &mut consumed, of_e.nb_bits());
            if matches!(
                reload(src_ptr, &mut ip, &mut win, &mut consumed),
                Reload::Overflow
            ) {
                return Err(DecompressBlockError::DecodeSequenceError(
                    DecodeSequenceError::NotEnoughBytesForNumSequences,
                ));
            }
        }

        let o_lit_end = op + ll;
        let o_match_end = o_lit_end + ml;
        let i_lit_end = lit + ll;
        if i_lit_end > lit_end || o_match_end > oend_w {
            op = exec_sequence_end::<NOWRAP>(
                op,
                ll,
                ml,
                offset,
                &mut lit,
                lit_base,
                lit_end,
                out_base,
                out_end,
                prefix_start,
                virtual_start,
                folded_base,
                view,
                HEADROOM,
            )
            .map_err(DecompressBlockError::ExecuteSequencesError)?;
            continue;
        }
        // Literals copy 16 bytes unconditionally: a zero length writes
        // garbage the match overwrites, and a branch on it would mispredict
        // on every shape with short literal runs.
        // SAFETY: `o_match_end <= oend_w` keeps every 16-byte store inside
        // the target or its slack. The 15-byte over-read past the literals
        // end stays defined on both literal sources: the scratch staging
        // buffer's zeroed tail (`zero_wildcopy_tail`), or the output
        // buffer's own bytes past an in-dst staged region, which the
        // placement's tail gate keeps readable.
        unsafe {
            copy16(op as *mut u8, lit as *const u8);
            if ll > 16 {
                wildcopy16((op + 16) as *mut u8, (lit + 16) as *const u8, ll - 16);
            }
        }
        lit = i_lit_end;
        #[cfg(feature = "seq_dump")]
        super::seq_dump::record_exec(
            (o_lit_end - out_base as usize) as u64,
            ml as u32,
            offset as u64,
        );
        // One compare covers every in-generation source; the window check
        // and the wrapped-source copy only run behind it. The never-wrapped
        // instantiation has `prefix_start == virtual_start`, so every
        // source it sends here is out of the window.
        if offset > o_lit_end.wrapping_sub(prefix_start) {
            let reach = o_lit_end.wrapping_sub(virtual_start);
            if NOWRAP || offset > reach {
                return Err(DecompressBlockError::ExecuteSequencesError(offset_too_big(
                    offset, reach,
                )));
            }
            copy_wrapped_match(
                *view,
                folded_base,
                out_base,
                o_lit_end - out_base as usize,
                offset,
                ml,
                true,
            )
            .map_err(DecompressBlockError::ExecuteSequencesError)?;
            op = o_match_end;
            continue;
        }
        // SAFETY: the source sits `offset` bytes behind the destination
        // inside the active generation, and the budget above covers the
        // wildcopy overshoot.
        unsafe {
            wildcopy_match(o_lit_end as *mut u8, (o_lit_end - offset) as *const u8, ml);
        }
        op = o_match_end;
    }

    dec.ip = ip;
    dec.win = win;
    dec.consumed = consumed;
    dec.ll_state = ll_state;
    dec.ml_state = ml_state;
    dec.of_state = of_state;
    dec.rem = rem;
    offset_hist[0] = rep0 as u32;
    offset_hist[1] = rep1 as u32;
    offset_hist[2] = rep2 as u32;

    let rest = lit_end - lit;
    if rest > 0 {
        if op + rest > out_end {
            return Err(DecompressBlockError::ExecuteSequencesError(
                ExecuteSequencesError::TargetTooSmall,
            ));
        }
        // SAFETY: budget checked above; plain copy, no overshoot
        unsafe {
            core::ptr::copy_nonoverlapping(lit as *const u8, op as *mut u8, rest);
        }
        op += rest;
    }
    *written = op - out_base as usize;
    dec.finish()
        .map_err(DecompressBlockError::DecodeSequenceError)
}

/// Take `n` bits, 1 to 31, from the pre-shifted window.
#[inline(always)]
fn read_fast(win: &mut u64, consumed: &mut u32, n: u32) -> usize {
    let v = *win >> (64 - n);
    *win <<= n;
    *consumed += n;
    v as usize
}

/// Take `n` bits, 0 to 9, from the pre-shifted window. The mask turns the
/// zero-width shift's garbage into 0.
#[inline(always)]
fn read_bits(win: &mut u64, consumed: &mut u32, n: u32) -> u32 {
    let v = (*win).wrapping_shr(64 - n) & ((1u64 << n) - 1);
    *win <<= n;
    *consumed += n;
    v as u32
}

#[cold]
fn offset_too_big(offset: usize, reach: usize) -> ExecuteSequencesError {
    ExecuteSequencesError::DecodebufferError(
        crate::decoding::errors::DecodeBufferError::OffsetTooBig {
            offset,
            buf_len: reach,
        },
    )
}

/// Copy `len` bytes in 16-byte chunks, overshooting the end by up to 15.
#[inline(always)]
unsafe fn wildcopy16(mut d: *mut u8, mut s: *const u8, len: usize) {
    // SAFETY: the caller's budget covers `len + 15` bytes at both cursors.
    unsafe {
        let end = d.add(len);
        while d < end {
            copy16(d, s);
            d = d.add(16);
            s = s.add(16);
        }
    }
}

/// Slow path for sequences near the target end or past the literals:
/// exact copies, with the bounds checked in libzstd's order.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn exec_sequence_end<const NOWRAP: bool>(
    op: usize,
    ll: usize,
    ml: usize,
    offset: usize,
    lit: &mut usize,
    lit_base: usize,
    lit_end: usize,
    out_base: *mut u8,
    out_end: usize,
    prefix_start: usize,
    virtual_start: usize,
    folded_base: usize,
    view: &crate::decoding::flat_buffer::FlatView,
    headroom: bool,
) -> Result<usize, ExecuteSequencesError> {
    let o_lit_end = op + ll;
    let o_match_end = o_lit_end + ml;
    if o_match_end > out_end {
        return Err(ExecuteSequencesError::TargetTooSmall);
    }
    if *lit + ll > lit_end {
        return Err(ExecuteSequencesError::NotEnoughBytesForSequence {
            wanted: *lit - lit_base + ll,
            have: lit_end - lit_base,
        });
    }
    // SAFETY: both ranges were bounds-checked above
    unsafe { core::ptr::copy_nonoverlapping(*lit as *const u8, op as *mut u8, ll) };
    *lit += ll;
    let reach = o_lit_end.wrapping_sub(virtual_start);
    if offset > reach {
        return Err(offset_too_big(offset, reach));
    }
    #[cfg(feature = "seq_dump")]
    super::seq_dump::record_exec(
        (o_lit_end - out_base as usize) as u64,
        ml as u32,
        offset as u64,
    );
    if !NOWRAP && offset > o_lit_end.wrapping_sub(prefix_start) {
        copy_wrapped_match(
            *view,
            folded_base,
            out_base,
            o_lit_end - out_base as usize,
            offset,
            ml,
            headroom,
        )?;
        return Ok(o_match_end);
    }
    let dst = o_lit_end as *mut u8;
    // SAFETY: the source sits `offset` bytes behind `dst` inside the target,
    // and each chunk reads only bytes already written.
    unsafe {
        let src = dst.sub(offset);
        let mut copied = 0;
        while copied < ml {
            let chunk = (offset + copied).min(ml - copied);
            core::ptr::copy(src, dst.add(copied), chunk);
            copied += chunk;
        }
    }
    Ok(o_match_end)
}

/// Inline 8-byte copy: variable-length libc calls cost ~15-25 cycles of
/// PLT and prologue each, which dominated the fused loop on sequence-dense
/// payloads (millions of ~5-byte copies per 32 MiB). Fixed 8/16-byte
/// load-store pairs have no such overhead. Both pointers must allow 8
/// readable/writable bytes (the wildcopy budget checks provide them).
#[inline(always)]
unsafe fn copy8(dst: *mut u8, src: *const u8) {
    // SAFETY: the caller guarantees 8 readable bytes at `src` and 8
    // writable bytes at `dst`.
    unsafe {
        dst.cast::<u64>()
            .write_unaligned(src.cast::<u64>().read_unaligned());
    }
}

/// Inline 16-byte copy (see [`copy8`]).
#[inline(always)]
unsafe fn copy16(dst: *mut u8, src: *const u8) {
    // SAFETY: the caller guarantees 16 readable bytes at `src` and 16
    // writable bytes at `dst`.
    unsafe {
        dst.cast::<u128>()
            .write_unaligned(src.cast::<u128>().read_unaligned());
    }
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
    // SAFETY: the caller's contract (distance 1..16, 8 readable/writable
    // bytes at both cursors) bounds every access below; the d+4 load only
    // reads bytes the stores above already wrote.
    unsafe {
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
pub(crate) unsafe fn wildcopy_match(mut d: *mut u8, mut s: *const u8, ml: usize) {
    // SAFETY: the caller's budget (`end + 16 <= out_len`) keeps the 16-byte
    // chunk overshoot inside the output buffer, and the source distance
    // contract keeps every load behind the write cursor.
    unsafe {
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
}

/// Match copy for a source at or below the active generation (see
/// `FlatView`): the common case resolves inline at the top as one linear
/// copy (see the fast path below); sources crossing the generation boundary
/// fall to the generic walk, split at the physical edges with doubling out
/// of already written bytes. Out of line — the segment-mapping state is
/// loop-invariant but keeping it live in the fused loop costs every
/// sequence register pressure, and the straddling path is rare outside
/// long-offset-heavy frames.
#[cold]
fn copy_wrapped_match(
    view: crate::decoding::flat_buffer::FlatView,
    folded_base: usize,
    out_ptr: *mut u8,
    pos: usize,
    offset: usize,
    ml: usize,
    wild: bool,
) -> Result<(), ExecuteSequencesError> {
    use crate::decoding::errors::{DecodeBufferError, ExecuteSequencesError};
    let crate::decoding::flat_buffer::FlatView {
        origin,
        prev_origin,
        prev_phys_base,
        seg_a_end,
        out_len: seg_out_len,
    } = view;
    // Un-fold the generation floor the caller's `vbase_op` carries: every
    // virtual address below is a true global position.
    let virt_base = folded_base.wrapping_add(prev_origin);
    let block_start = virt_base.saturating_sub(origin);
    // The dispatch in `exec_one_flat` (`offset.wrapping_add(wrap_base) >
    // dst_a`, i.e. `offset > block_start + pos`) admits only sources
    // strictly below the physical buffer start; the reach
    // `offset - block_start - pos` below relies on that to stay positive.
    debug_assert!(offset > block_start.saturating_add(pos));
    // Fast path: a source entirely inside the previous generation is still
    // one linear in-buffer copy, reading the previous generation's intact
    // top. `d` is how far the source reaches below the active generation's
    // start: d >= ml means the match does not straddle. The second guard
    // (`offset + ml <= seg_a_end`, i.e. the source stays `ml` under the
    // previous generation's physical end) makes the source region sit at or
    // above the destination's end (a non-overlapping forward copy, and
    // wildcopy reads end within `buf.len()`), mirroring the physical layout
    // of libzstd's ring after a wrap. Straddling or deeper sources fall to
    // the generic walk, which rejects anything below `prev_origin`.
    let d = offset - block_start - pos;
    if d >= ml && offset + ml <= seg_a_end {
        // SAFETY: bounds as documented above
        unsafe {
            let dst = out_ptr.add(pos);
            let src = dst.add(prev_phys_base + seg_a_end - offset);
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
        // Map into the active generation, or into the previous generation
        // below its physical end; anything lower is out of window.
        let (src_abs, seg_end, active) = if src_v >= origin {
            (src_v - origin, seg_out_len, true)
        } else if src_v >= prev_origin {
            (
                prev_phys_base + (src_v - prev_origin),
                prev_phys_base + seg_a_end,
                false,
            )
        } else {
            return Err(ExecuteSequencesError::DecodebufferError(
                DecodeBufferError::OffsetTooBig {
                    offset,
                    buf_len: virt_base + pos - prev_origin,
                },
            ));
        };
        // Both mappings land strictly inside their segment: the active one
        // below the physical write cursor (sources sit behind the
        // destination), the previous one below its physical end by the
        // chain identity `origin == prev_origin + seg_a_end`. Pins the
        // `seg_end - src_abs` subtraction below against a mapping bug.
        debug_assert!(src_abs < seg_end);
        let chunk = remaining.min(seg_end - src_abs);
        if active && wild {
            // The whole rest of the match lies in the active generation
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
/// "actual" offset needed because offsets are not stored in a raw way, some transformations are
/// needed before you get a functional number. The return is `None` exactly when the resolution
/// lands on offset 0 (a malformed dictionary seeding the history), which every caller rejects;
/// every other resolution is nonzero by construction, so the `NonZero` type carries that past
/// the boundary.
///
/// Real offsets (`offset_value > 3`, the 87-100% share on json/dll/skewed)
/// take a constant 3-move rotation and return immediately (R38): they need
/// no slot machinery, and the fast-path branch sits on the offset class,
/// which predicts. The repcode domain (`offset_value <= 3`) keeps the
/// ZSTD_decodeSequence-style select form: the code plus the
/// literal-length-0 flag forms a slot index (`code - 1 + ll0`), where
/// slot 3 — code 3 with no literals — is the `rep0 - 1` pseudo-slot,
/// folded onto `scratch[0]` with a conditional subtract after the shared
/// load. The same slot picks the history rotation: 0 keeps everything
/// (code 1 with literals), anything else rotates all three.
///
/// The repcode domain stays entirely select-based: repcodes flip too often
/// on structured input for the rotation match to predict, and it sat among
/// the largest branch-miss clusters of the fused decode loop before the
/// select rewrite. The slot load stays in bounds by masking the (garbage)
/// index, whose loaded value the selects then discard.
pub(crate) fn do_offset_history(
    offset_value: u32,
    lit_len: u32,
    scratch: &mut [u32; 3],
) -> Option<NonZero<u32>> {
    // Real offsets (the 87-100% share on json/dll/skewed) need no slot
    // machinery: a constant 3-move rotation, off the select chain the
    // repcode domain keeps.
    if offset_value > 3 {
        let actual = offset_value - 3;
        scratch[2] = scratch[1];
        scratch[1] = scratch[0];
        scratch[0] = actual;
        debug_assert!(actual != 0);
        // SAFETY: offset_value > 3 proves actual = offset_value - 3 >= 1.
        return Some(unsafe { NonZero::new_unchecked(actual) });
    }
    let idx = offset_value
        .wrapping_sub(1)
        .wrapping_add((lit_len == 0) as u32);
    // idx in 0..=3 exactly for repcodes; (idx & 3) with 3 folded back to 0
    // is the pseudo-slot mapping
    let slot = (idx & 3) as usize;
    let slot = if slot == 3 {
        0
    } else {
        slot
    };
    let actual_offset = if offset_value <= 3 {
        // A malformed dictionary can seed scratch[0] with 0; saturate so this
        // resolves to 0 (rejected upstream as ZeroOffset) instead of
        // underflowing. See #115.
        scratch[slot].saturating_sub((idx == 3) as u32)
    } else {
        offset_value.wrapping_sub(3)
    };

    // Rotation as selects: slot 0 keeps everything, slot 1 swaps the two
    // most recent codes (slot 2 untouched), anything else rotates all
    // three. Ordered so every read sees the pre-update values. Real offsets
    // never reach here (the fast path above returned).
    let keep = idx == 0;
    let rotate_all = idx >= 2;
    scratch[2] = if rotate_all {
        scratch[1]
    } else {
        scratch[2]
    };
    scratch[1] = if keep {
        scratch[1]
    } else {
        scratch[0]
    };
    scratch[0] = if keep {
        scratch[0]
    } else {
        actual_offset
    };

    NonZero::new(actual_offset)
}

#[cfg(test)]
mod tests {
    use core::num::NonZero;

    use super::do_offset_history;

    #[test]
    fn repeat_offset_minus_one_with_zero_history_does_not_underflow() {
        // A malformed dictionary can seed offset history slot 0 with 0. With
        // literal length 0 and offset code 3 ("repeat the most recent offset,
        // minus one"), `scratch[0] - 1` must not underflow; it should resolve to
        // 0, which the caller rejects as ExecuteSequencesError::ZeroOffset rather
        // than panicking (debug) or wrapping to u32::MAX (release). See #115.
        let mut scratch = [0u32, 4, 8];
        assert_eq!(do_offset_history(3, 0, &mut scratch), None);
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
            (true, 1) => {},
            (true, 2) | (false, 1) => {
                s[1] = s[0];
                s[0] = actual;
            },
            _ => {
                s[2] = s[1];
                s[1] = s[0];
                s[0] = actual;
            },
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
                    // The reference's plain-u32 form maps 0 onto `None`.
                    assert_eq!(
                        (ra, a),
                        (NonZero::new(rb), b),
                        "of={of} ll={ll} seed={seed}"
                    );
                }
            }
        }
        // The zero-seeded pseudo-slot must saturate identically (see #115).
        for of in 1u32..6 {
            let mut a = [0u32, 4, 8];
            let mut b = a;
            assert_eq!(
                do_offset_history(of, 0, &mut a),
                NonZero::new(reference(of, 0, &mut b)),
                "of={of}"
            );
            assert_eq!(a, b, "of={of}");
        }
    }
}
