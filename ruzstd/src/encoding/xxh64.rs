//! XXH64 for the frame content checksum, spec-exact and in-tree so the raw
//! block write can absorb input bytes while it copies them (the hash pass
//! otherwise costs a full read of every incompressible block). The bulk loop
//! keeps zstd's four independent accumulator chains live at once; a vector
//! version would serialize them behind `vpmullq` latency and lose.

const PRIME1: u64 = 11400714785074694791;
const PRIME2: u64 = 14029467366897019727;
const PRIME3: u64 = 1609587929392839161;
const PRIME4: u64 = 9650029242287828579;
const PRIME5: u64 = 2870177450012600261;

use alloc::vec::Vec;
use core::convert::TryInto;

#[inline(always)]
fn round(acc: u64, lane: u64) -> u64 {
    (acc.wrapping_add(lane.wrapping_mul(PRIME2)))
        .rotate_left(31)
        .wrapping_mul(PRIME1)
}

#[inline(always)]
fn merge_round(mut acc: u64, val: u64) -> u64 {
    acc ^= round(0, val);
    acc.wrapping_mul(PRIME1).wrapping_add(PRIME4)
}

#[inline(always)]
fn read64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

#[inline(always)]
fn read32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

/// Streaming XXH64 (seed 0 is all the frame checksum needs; kept general).
pub(crate) struct Xxh64 {
    v: [u64; 4],
    total: u64,
    mem: [u8; 32],
    mem_len: usize,
}

impl Xxh64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            v: [
                seed.wrapping_add(PRIME1).wrapping_add(PRIME2),
                seed.wrapping_add(PRIME2),
                seed,
                seed.wrapping_sub(PRIME1),
            ],
            total: 0,
            mem: [0; 32],
            mem_len: 0,
        }
    }

    /// Absorb one 32-byte block into the accumulators. Caller guarantees
    /// `chunk.len() == 32`.
    #[inline(always)]
    fn absorb32(&mut self, chunk: &[u8]) {
        self.v[0] = round(self.v[0], read64(chunk));
        self.v[1] = round(self.v[1], read64(&chunk[8..]));
        self.v[2] = round(self.v[2], read64(&chunk[16..]));
        self.v[3] = round(self.v[3], read64(&chunk[24..]));
    }

    fn absorb(&mut self, mut input: &[u8]) {
        if self.mem_len > 0 {
            let fill = (32 - self.mem_len).min(input.len());
            self.mem[self.mem_len..self.mem_len + fill].copy_from_slice(&input[..fill]);
            self.mem_len += fill;
            input = &input[fill..];
            if self.mem_len < 32 {
                return;
            }
            let mem = self.mem;
            self.absorb32(&mem);
            self.mem_len = 0;
        }
        // The accumulator chains are serial, so throughput comes from
        // keeping more of them in flight: two 32-byte chunks per iteration
        // (eight chains) beats one by ~8% on this core.
        let mut v = self.v;
        while input.len() >= 64 {
            // SAFETY: the loop guard bounds both chunk reads.
            unsafe {
                let a = input.as_ptr();
                let b = a.add(32);
                v[0] = round(v[0], a.cast::<u64>().read_unaligned());
                v[1] = round(v[1], a.add(8).cast::<u64>().read_unaligned());
                v[2] = round(v[2], a.add(16).cast::<u64>().read_unaligned());
                v[3] = round(v[3], a.add(24).cast::<u64>().read_unaligned());
                v[0] = round(v[0], b.cast::<u64>().read_unaligned());
                v[1] = round(v[1], b.add(8).cast::<u64>().read_unaligned());
                v[2] = round(v[2], b.add(16).cast::<u64>().read_unaligned());
                v[3] = round(v[3], b.add(24).cast::<u64>().read_unaligned());
            }
            input = &input[64..];
        }
        self.v = v;
        while input.len() >= 32 {
            let chunk = input;
            self.absorb32(chunk);
            input = &input[32..];
        }
        self.mem[..input.len()].copy_from_slice(input);
        self.mem_len = input.len();
    }

    /// Whether the stream sits at a 32-byte boundary (no buffered tail);
    /// [`Self::scan_uniform_absorbing`] needs this.
    #[inline(always)]
    pub(crate) fn mem_is_empty(&self) -> bool {
        self.mem_len == 0
    }

    pub(crate) fn write(&mut self, input: &[u8]) {
        self.total += input.len() as u64;
        self.absorb(input);
    }

    /// Append `input` to `out` while absorbing it: one combined loop instead
    /// of a hash read pass plus a separate copy pass. Byte-identical output
    /// to `out.extend_from_slice(input); self.write(input)`.
    pub(crate) fn write_appending(&mut self, out: &mut Vec<u8>, input: &[u8]) {
        self.write_appending_from(out, input, 0);
    }

    /// Append all of `input` to `out`, absorbing only `input[hash_from..]`:
    /// the prefix's checksum was already absorbed elsewhere (the fused
    /// uniform scan). `hash_from` must be a multiple of 32 (or the length),
    /// and the buffer must be empty when it is nonzero.
    pub(crate) fn write_appending_from(
        &mut self,
        out: &mut Vec<u8>,
        input: &[u8],
        hash_from: usize,
    ) {
        // The [0, hash_from) prefix was counted when the fused scan
        // absorbed it; only the resumed range adds to the length.
        self.total += (input.len() - hash_from) as u64;
        out.reserve(input.len());
        let base = out.len();
        // SAFETY: the reserve above covers exactly the stored range
        // [base, base + input.len()); source reads are guarded by the same
        // length and both pointers advance together.
        unsafe {
            let src = input.as_ptr();
            let dst = out.as_mut_ptr().add(base);
            let mut i = 0usize;
            if self.mem_len > 0 {
                debug_assert_eq!(hash_from, 0, "resumed writes need an empty buffer");
                // Buffer the head through `mem` so the stream stays aligned
                // to 32-byte boundaries; the bytes still reach `out`.
                let fill = (32 - self.mem_len).min(input.len());
                core::ptr::copy_nonoverlapping(src, dst, fill);
                self.mem[self.mem_len..self.mem_len + fill]
                    .copy_from_slice(input.get_unchecked(..fill));
                self.mem_len += fill;
                i = fill;
                if self.mem_len == 32 {
                    let mem = self.mem;
                    self.absorb32(&mem);
                    self.mem_len = 0;
                }
            }
            if hash_from > 0 {
                // Copy-only: the checksum of this range is already absorbed.
                core::ptr::copy_nonoverlapping(src, dst, hash_from);
                i = hash_from;
            }
            let mut v = self.v;
            // Two chunks per iteration keeps eight accumulator chains in
            // flight (see `absorb`); the copy reuses the loaded words.
            while i + 64 <= input.len() {
                let p = src.add(i);
                let q = src.add(i + 32);
                let a0 = p.cast::<u64>().read_unaligned();
                let a1 = p.add(8).cast::<u64>().read_unaligned();
                let a2 = p.add(16).cast::<u64>().read_unaligned();
                let a3 = p.add(24).cast::<u64>().read_unaligned();
                let b0 = q.cast::<u64>().read_unaligned();
                let b1 = q.add(8).cast::<u64>().read_unaligned();
                let b2 = q.add(16).cast::<u64>().read_unaligned();
                let b3 = q.add(24).cast::<u64>().read_unaligned();
                v[0] = round(v[0], a0);
                v[1] = round(v[1], a1);
                v[2] = round(v[2], a2);
                v[3] = round(v[3], a3);
                v[0] = round(v[0], b0);
                v[1] = round(v[1], b1);
                v[2] = round(v[2], b2);
                v[3] = round(v[3], b3);
                let d = dst.add(i);
                d.cast::<u64>().write_unaligned(a0);
                d.add(8).cast::<u64>().write_unaligned(a1);
                d.add(16).cast::<u64>().write_unaligned(a2);
                d.add(24).cast::<u64>().write_unaligned(a3);
                let e = dst.add(i + 32);
                e.cast::<u64>().write_unaligned(b0);
                e.add(8).cast::<u64>().write_unaligned(b1);
                e.add(16).cast::<u64>().write_unaligned(b2);
                e.add(24).cast::<u64>().write_unaligned(b3);
                i += 64;
            }
            while i + 32 <= input.len() {
                let p = src.add(i);
                let c0 = p.cast::<u64>().read_unaligned();
                let c1 = p.add(8).cast::<u64>().read_unaligned();
                let c2 = p.add(16).cast::<u64>().read_unaligned();
                let c3 = p.add(24).cast::<u64>().read_unaligned();
                v[0] = round(v[0], c0);
                v[1] = round(v[1], c1);
                v[2] = round(v[2], c2);
                v[3] = round(v[3], c3);
                let d = dst.add(i);
                d.cast::<u64>().write_unaligned(c0);
                d.add(8).cast::<u64>().write_unaligned(c1);
                d.add(16).cast::<u64>().write_unaligned(c2);
                d.add(24).cast::<u64>().write_unaligned(c3);
                i += 32;
            }
            self.v = v;
            let rest = input.len() - i;
            if rest > 0 {
                core::ptr::copy_nonoverlapping(src.add(i), dst.add(i), rest);
                self.mem[..rest].copy_from_slice(input.get_unchecked(i..));
                self.mem_len = rest;
            }
            out.set_len(base + input.len());
        }
    }

    /// Uniform scan fused with the checksum absorb: one pass compares every
    /// word against a broadcast of the first byte while running the hash
    /// rounds on the same loads. Returns `(uniform, consumed)`: exactly the
    /// first `consumed` bytes are absorbed — a multiple of 32 while a
    /// mismatch remains possible, the whole input once the verdict is
    /// uniform — so nothing scanned is ever wasted; the caller resumes the
    /// checksum at `consumed` (the raw-block copy fuses that resume into
    /// its own loop). Requires an empty buffer (stream position at a
    /// 32-byte boundary).
    pub(crate) fn scan_uniform_absorbing(&mut self, input: &[u8]) -> (bool, usize) {
        debug_assert_eq!(self.mem_len, 0);
        let Some(&first) = input.first() else {
            return (true, 0);
        };
        let br = u64::from(first) * 0x0101_0101_0101_0101;
        let len = input.len();
        let base = input.as_ptr();
        let mut v = self.v;
        let mut i = 0usize;
        let mut uniform = true;
        // SAFETY: every read is guarded by its loop condition; the rounds
        // absorb exactly the loaded words.
        unsafe {
            'outer: while i + 64 <= len {
                let p = base.add(i);
                let q = p.add(32);
                let a0 = p.cast::<u64>().read_unaligned();
                let a1 = p.add(8).cast::<u64>().read_unaligned();
                let a2 = p.add(16).cast::<u64>().read_unaligned();
                let a3 = p.add(24).cast::<u64>().read_unaligned();
                let b0 = q.cast::<u64>().read_unaligned();
                let b1 = q.add(8).cast::<u64>().read_unaligned();
                let b2 = q.add(16).cast::<u64>().read_unaligned();
                let b3 = q.add(24).cast::<u64>().read_unaligned();
                v[0] = round(v[0], a0);
                v[1] = round(v[1], a1);
                v[2] = round(v[2], a2);
                v[3] = round(v[3], a3);
                v[0] = round(v[0], b0);
                v[1] = round(v[1], b1);
                v[2] = round(v[2], b2);
                v[3] = round(v[3], b3);
                i += 64;
                if (a0 ^ br) | (a1 ^ br) | (a2 ^ br) | (a3 ^ br)
                    | (b0 ^ br) | (b1 ^ br) | (b2 ^ br) | (b3 ^ br)
                    != 0
                {
                    uniform = false;
                    break 'outer;
                }
            }
            if uniform && i + 32 <= len {
                let p = base.add(i);
                let c0 = p.cast::<u64>().read_unaligned();
                let c1 = p.add(8).cast::<u64>().read_unaligned();
                let c2 = p.add(16).cast::<u64>().read_unaligned();
                let c3 = p.add(24).cast::<u64>().read_unaligned();
                v[0] = round(v[0], c0);
                v[1] = round(v[1], c1);
                v[2] = round(v[2], c2);
                v[3] = round(v[3], c3);
                i += 32;
                if (c0 ^ br) | (c1 ^ br) | (c2 ^ br) | (c3 ^ br) != 0 {
                    uniform = false;
                }
            }
        }
        self.v = v;
        self.total += i as u64;
        if !uniform {
            return (false, i);
        }
        // Sub-32-byte tail: on mismatch it stays unabsorbed (the caller
        // resumes at `i`), otherwise it is buffered into `mem` and the
        // whole block counts as hashed. The verdict compares against the
        // block's first byte, not merely internal uniformity.
        let tail = &input[i..];
        if !tail.iter().all(|&b| b == first) {
            return (false, i);
        }
        self.absorb(tail);
        self.total += tail.len() as u64;
        (true, len)
    }

    pub(crate) fn finish(&self) -> u64 {
        let mut h = if self.total >= 32 {
            let [v0, v1, v2, v3] = self.v;
            let acc = v0.rotate_left(1)
                .wrapping_add(v1.rotate_left(7))
                .wrapping_add(v2.rotate_left(12))
                .wrapping_add(v3.rotate_left(18));
            merge_round(merge_round(merge_round(merge_round(acc, v0), v1), v2), v3)
        } else {
            PRIME5
        };
        h = h.wrapping_add(self.total);

        let mut tail = &self.mem[..self.mem_len];
        while tail.len() >= 8 {
            h ^= round(0, read64(tail));
            h = h.rotate_left(27).wrapping_mul(PRIME1).wrapping_add(PRIME4);
            tail = &tail[8..];
        }
        if tail.len() >= 4 {
            h ^= (read32(tail) as u64).wrapping_mul(PRIME1);
            h = h.rotate_left(23).wrapping_mul(PRIME2).wrapping_add(PRIME3);
            tail = &tail[4..];
        }
        for &b in tail {
            h ^= (b as u64).wrapping_mul(PRIME5);
            h = h.rotate_left(11).wrapping_mul(PRIME1);
        }

        h ^= h >> 33;
        h = h.wrapping_mul(PRIME2);
        h ^= h >> 29;
        h = h.wrapping_mul(PRIME3);
        h ^= h >> 32;
        h
    }
}

#[cfg(test)]
mod tests {
    use super::Xxh64;
    use alloc::vec::Vec;
    use core::hash::Hasher;

    /// Empty and single-byte inputs hit the no-bulk tail paths; values from
    /// twox-hash, which the decoder's checksum verification still uses.
    #[test]
    fn tiny_inputs_match_twox() {
        let mut reference = twox_hash::XxHash64::with_seed(0);
        reference.write(&[]);
        let mut h = Xxh64::new(0);
        h.write(&[]);
        assert_eq!(h.finish(), reference.finish());

        let mut reference = twox_hash::XxHash64::with_seed(0);
        reference.write(&[0]);
        let mut h = Xxh64::new(0);
        h.write(&[0]);
        assert_eq!(h.finish(), reference.finish());
    }

    /// Match twox-hash byte for byte across sizes and streaming splits: the
    /// fused append path and the plain write path must agree with it.
    #[test]
    fn matches_twox_over_lengths_and_splits() {
        let mut state = 0x123456789ABCDEF0u64;
        let mut data = Vec::with_capacity(70_000);
        while data.len() < 70_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(&state.to_le_bytes());
        }
        let mut splits = Vec::new();
        for len in [0usize, 1, 3, 4, 7, 8, 9, 15, 16, 31, 32, 33, 63, 64, 65, 71, 100, 999, 1024, 2048, 4096, 32 * 1024, 70_000] {
            let mut reference = twox_hash::XxHash64::with_seed(0);
            reference.write(&data[..len]);
            splits.push((len, reference.finish()));
        }
        for (len, want) in splits {
            let mut h = Xxh64::new(0);
            h.write(&data[..len]);
            assert_eq!(h.finish(), want, "plain write at len {len}");

            // every 7-byte chunk: exercises the streaming mem buffer
            let mut h = Xxh64::new(0);
            for chunk in data[..len].chunks(7) {
                h.write(chunk);
            }
            assert_eq!(h.finish(), want, "chunked write at len {len}");

            // fused append: same hash and same bytes out
            let mut h = Xxh64::new(0);
            let mut out = Vec::new();
            h.write_appending(&mut out, &data[..len.max(1) - 1]);
            h.write_appending(&mut out, &data[len.max(1) - 1..len]);
            assert_eq!(h.finish(), want, "fused append at len {len}");
            assert_eq!(out.len(), len);
            assert_eq!(out, data[..len]);
        }
    }

    /// The fused uniform scan plus its resume must equal one plain write,
    /// and the resumed append must produce the same hash and bytes.
    #[test]
    fn scan_uniform_resume_matches_plain_write() {
        let uniform = alloc::vec![7u8; 1000];
        for len in [0usize, 1, 7, 26, 31, 32, 33, 63, 64, 65, 96, 128, 999, 1000] {
            let u = &uniform[..len];
            // same length with the final byte flipped (uniform for len <= 1)
            let mut mismatched = alloc::vec![7u8; len];
            if len > 0 {
                mismatched[len - 1] ^= 1;
            }
            let m = &mismatched[..];

            let (is_uniform, hashed) = {
                let mut scan = Xxh64::new(0);
                let r = scan.scan_uniform_absorbing(u);
                assert!(r.0, "uniform at len {len}");
                scan.finish();
                r
            };
            assert!(is_uniform);
            let mut plain = Xxh64::new(0);
            plain.write(u);
            let mut scan = Xxh64::new(0);
            let (_, hashed) = scan.scan_uniform_absorbing(u);
            assert_eq!(hashed, len);
            assert_eq!(scan.finish(), plain.finish(), "commit at len {len}");

            let mut scan = Xxh64::new(0);
            let (uni, hashed) = scan.scan_uniform_absorbing(m);
            assert_eq!(uni, len <= 1, "mismatch at len {len}");
            if !uni {
                assert!(hashed % 32 == 0 && hashed <= len);
            }
            scan.write(&m[hashed..]);
            let mut plain = Xxh64::new(0);
            plain.write(m);
            assert_eq!(scan.finish(), plain.finish(), "resume at len {len}");

            // the raw-copy resume: full copy, hash from the resume offset
            // (only the mismatch case reaches a raw copy — uniform blocks
            // take the RLE path and never resume)
            let mut scan = Xxh64::new(0);
            let (uni, hashed) = scan.scan_uniform_absorbing(m);
            if !uni {
                let mut out = Vec::new();
                scan.write_appending_from(&mut out, m, hashed);
                assert_eq!(out, m);
                assert_eq!(scan.finish(), plain.finish(), "append resume at len {len}");
            }
        }
    }
}
