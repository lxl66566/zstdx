//! Gear-hash long-distance matcher (port of libzstd's `zstd_ldm.c`): a
//! sparse candidate source for distance classes the dense search tables
//! lose to pressure. A rolling gear hash samples the input at ~one split
//! per `2^HASH_RATE_LOG` bytes; each split's 64-byte window is bucketed by
//! its XXH64 into a small round-robin table (16 entries per bucket). The
//! table holds exactly one window of splits, so a far twin of the window's
//! content survives where a per-position hash table buries it under near
//! recurrences.
//!
//! Deviations from C, all behavior-preserving:
//! - entries are `pack_pos`-biased u32 positions (the chain-table idiom) instead of raw u32
//!   offsets, so an unwritten slot resolves as an out-of-range distance instead of aliasing
//!   position 0;
//! - the rolling state is carried across blocks (C re-arms per 1 MiB chunk, leaving a 64-byte blind
//!   spot each time); the arm position is tracked as an entry floor so no split window reaches
//!   before it;
//! - a match's backward extension is not measured here: sequences carry only (split, offset) and
//!   the consumer recomputes length live against its own anchor — the same bytes under a tighter
//!   bound.

use alloc::vec::Vec;

use super::match_generator::{extend_match, pack_pos};
use crate::xxh64::Xxh64;

/// Shortest split-window match that can become a candidate (C's
/// `LDM_MIN_MATCH_LENGTH` for the lazy strategies).
pub(super) const MIN_MATCH_LENGTH: usize = 64;
/// Average bytes between split points (C's lazy2-family `hashRateLog`).
const HASH_RATE_LOG: u32 = 6;
/// Entries per bucket (C's `LDM_BUCKET_SIZE_LOG`).
const BUCKET_SIZE_LOG: u32 = 4;
const ENTS_PER_BUCKET: usize = 1 << BUCKET_SIZE_LOG;
/// Splits staged per gear-feed call (C's `LDM_BATCH_SIZE`).
const BATCH_SIZE: usize = 64;

/// libzstd's `ZSTD_ldm_gearTab`: one random u64 per byte; bit n of the
/// rolling hash depends on the last n bytes, so the split mask reads the
/// highest-weight bits of the 64-byte window.
static GEAR_TAB: [u64; 256] = [
    0xf5b8f72c5f77775c,
    0x84935f266b7ac412,
    0xb647ada9ca730ccc,
    0xb065bb4b114fb1de,
    0x34584e7e8c3a9fd0,
    0x4e97e17c6ae26b05,
    0x3a03d743bc99a604,
    0xcecd042422c4044f,
    0x76de76c58524259e,
    0x9c8528f65badeaca,
    0x86563706e2097529,
    0x2902475fa375d889,
    0xafb32a9739a5ebe6,
    0xce2714da3883e639,
    0x21eaf821722e69e,
    0x37b628620b628,
    0x49a8d455d88caf5,
    0x8556d711e6958140,
    0x4f7ae74fc605c1f,
    0x829f0c3468bd3a20,
    0x4ffdc885c625179e,
    0x8473de048a3daf1b,
    0x51008822b05646b2,
    0x69d75d12b2d1cc5f,
    0x8c9d4a19159154bc,
    0xc3cc10f4abbd4003,
    0xd06ddc1cecb97391,
    0xbe48e6e7ed80302e,
    0x3481db31cee03547,
    0xacc3f67cdaa1d210,
    0x65cb771d7c9f96cc,
    0x8eb27177055723dd,
    0xc789950d44cd94be,
    0x934feadc3705b12b,
    0x5e485f11edbdf182,
    0x1e2e2a46fd64767a,
    0x2969ca71d82efa7c,
    0x9d46e9935ebbba2e,
    0xe056b67e05e6822b,
    0x94d73f55739d03a0,
    0xcd7010bdb69b5a03,
    0x455ef9fcd79b82f4,
    0x869cb54a8749c161,
    0x38d1a4fa6185d225,
    0xb475166f94bbe9bb,
    0xa4143548720959f1,
    0x7aed4780ba6b26ba,
    0xd0ce264439e02312,
    0x84366d746078d508,
    0xa8ce973c72ed17be,
    0x21c323a29a430b01,
    0x9962d617e3a80ee,
    0xab0ce91d9c8cf75b,
    0x530e8ee6d19a4dbc,
    0x2ef68c0cf53f5d72,
    0xc03a681640a85506,
    0x496e4e9f9c310967,
    0x78580472b59b14a0,
    0x273824c23b388577,
    0x66bf923ad45cb553,
    0x47ae1a5a2492ba86,
    0x35e3045622919659,
    0x4765182a46870b6f,
    0x6cbab625e9099412,
    0xddac9a2e598522c1,
    0x7172086e666624f2,
    0xdf5003ca503b7837,
    0x88c0c1db78563d09,
    0x58d51865acfc289d,
    0x177671aec65224f1,
    0xfb79d8a241e967d7,
    0x2be1e101cad9a49a,
    0x6625682f6e29186b,
    0x399553457ac06e50,
    0x35dffb4c23abb74,
    0x429db2591f54aade,
    0xc52802a8037d1009,
    0x6acb27381f0b25f3,
    0xf45e2551ee4f823b,
    0x8b0ea2d99580c2f7,
    0x3bed519cbcb4e1e1,
    0xff452823dbb010a,
    0x9d42ed6143fdd267,
    0x5b9313c06257c57b,
    0xa114b8008b5e1442,
    0xc1fe311c11c13d4b,
    0x66e8763ea34c5568,
    0x8b982af1c262f05d,
    0xee8876faaa75fbb7,
    0x8a62a4d0d172bb2a,
    0xc13d94a3b7449a97,
    0x6dbbba9dc15d037c,
    0xc786101f1d92e0f1,
    0xd78681a907a0b79b,
    0xf61aaf2962c9abb9,
    0x2cfd16fcd3cb7ad9,
    0x868c5b6744624d21,
    0x25e650899c74ddd7,
    0xba042af4a7c37463,
    0x4eb1a539465a3eca,
    0xbe09dbf03b05d5ca,
    0x774e5a362b5472ba,
    0x47a1221229d183cd,
    0x504b0ca18ef5a2df,
    0xdffbdfbde2456eb9,
    0x46cd2b2fbee34634,
    0xf2aef8fe819d98c3,
    0x357f5276d4599d61,
    0x24a5483879c453e3,
    0x88026889192c24b9,
    0x28da96671782dbec,
    0x4ef37c40588e9aaa,
    0x8837b90651bc9fb3,
    0xc164f741d3f0e5d6,
    0xbc135a0a704b70ba,
    0x69cd868f7622ada,
    0xbc37ba89e0b9c0ab,
    0x47c14a01323552f6,
    0x4f00794bacee98bb,
    0x7107de7d637a69d5,
    0x88af793bb6f2255e,
    0xf3c6466b8799b598,
    0xc288c616aa7f3b59,
    0x81ca63cf42fca3fd,
    0x88d85ace36a2674b,
    0xd056bd3779238e97,
    0xe55c396c4e9dd32d,
    0xbefb504571e6c0a6,
    0x96ab32115e91e8cc,
    0xbf8acb18de8f38d1,
    0x66dae58801672606,
    0x833b6017872317fb,
    0xb87c16f2d1c92864,
    0xdb766a74e58b669c,
    0x89659f85c61417be,
    0xc8daad856011ea0c,
    0x76a4b565b6fe7eae,
    0xa469d085f6237312,
    0xaaf0365683a3e96c,
    0x4dbb746f8424f7b8,
    0x638755af4e4acc1,
    0x3d7807f5bde64486,
    0x17be6d8f5bbb7639,
    0x903f0cd44dc35dc,
    0x67b672eafdf1196c,
    0xa676ff93ed4c82f1,
    0x521d1004c5053d9d,
    0x37ba9ad09ccc9202,
    0x84e54d297aacfb51,
    0xa0b4b776a143445,
    0x820d471e20b348e,
    0x1874383cb83d46dc,
    0x97edeec7a1efe11c,
    0xb330e50b1bdc42aa,
    0x1dd91955ce70e032,
    0xa514cdb88f2939d5,
    0x2791233fd90db9d3,
    0x7b670a4cc50f7a9b,
    0x77c07d2a05c6dfa5,
    0xe3778b6646d0a6fa,
    0xb39c8eda47b56749,
    0x933ed448addbef28,
    0xaf846af6ab7d0bf4,
    0xe5af208eb666e49,
    0x5e6622f73534cd6a,
    0x297daeca42ef5b6e,
    0x862daef3d35539a6,
    0xe68722498f8e1ea9,
    0x981c53093dc0d572,
    0xfa09b0bfbf86fbf5,
    0x30b1e96166219f15,
    0x70e7d466bdc4fb83,
    0x5a66736e35f2a8e9,
    0xcddb59d2b7c1baef,
    0xd6c7d247d26d8996,
    0xea4e39eac8de1ba3,
    0x539c8bb19fa3aff2,
    0x9f90e4c5fd508d8,
    0xa34e5956fbaf3385,
    0x2e2f8e151d3ef375,
    0x173691e9b83faec1,
    0xb85a8d56bf016379,
    0x8382381267408ae3,
    0xb90f901bbdc0096d,
    0x7c6ad32933bcec65,
    0x76bb5e2f2c8ad595,
    0x390f851a6cf46d28,
    0xc3e6064da1c2da72,
    0xc52a0c101cfa5389,
    0xd78eaf84a3fbc530,
    0x3781b9e2288b997e,
    0x73c2f6dea83d05c4,
    0x4228e364c5b5ed7,
    0x9d7a3edf0da43911,
    0x8edcfeda24686756,
    0x5e7667a7b7a9b3a1,
    0x4c4f389fa143791d,
    0xb08bc1023da7cddc,
    0x7ab4be3ae529b1cc,
    0x754e6132dbe74ff9,
    0x71635442a839df45,
    0x2f6fb1643fbe52de,
    0x961e0a42cf7a8177,
    0xf3b45d83d89ef2ea,
    0xee3de4cf4a6e3e9b,
    0xcd6848542c3295e7,
    0xe4cee1664c78662f,
    0x9947548b474c68c4,
    0x25d73777a5ed8b0b,
    0xc915b1d636b7fc,
    0x21c2ba75d9b0d2da,
    0x5f6b5dcf608a64a1,
    0xdcf333255ff9570c,
    0x633b922418ced4ee,
    0xc136dde0b004b34a,
    0x58cc83b05d4b2f5a,
    0x5eb424dda28e42d2,
    0x62df47369739cd98,
    0xb4e0b42485e4ce17,
    0x16e1f0c1f9a8d1e7,
    0x8ec3916707560ebf,
    0x62ba2e2df2cc9db3,
    0xcbf9f4ff77d83a16,
    0x78d9d7d07d2bbcc4,
    0xef554ce1e02c41f4,
    0x8d7581127eccf94d,
    0xa9b53336cb3c8a05,
    0x38c42c0bf45c4f91,
    0x640893cdf4488863,
    0x80ec34bc575ea568,
    0x39f324f5b48eaa40,
    0xe9d9ed1f8eff527f,
    0x9224fc058cc5a214,
    0xbaba00b04cfe7741,
    0x309a9f120fcf52af,
    0xa558f3ec65626212,
    0x424bec8b7adabe2f,
    0x41622513a6aea433,
    0xb88da2d5324ca798,
    0xd287733b245528a4,
    0x9a44697e6d68aec3,
    0x7b1093be2f49bb28,
    0x50bbec632e3d8aad,
    0x6cd90723e1ea8283,
    0x897b9e7431b02bf3,
    0x219efdcb338a7047,
    0x3b0311f0a27c0656,
    0xdb17bf91c0db96e7,
    0x8cd4fd6b4e85a5b2,
    0xfab071056ba6409d,
    0x40d6fe831fa9dfd9,
    0xaf358debad7d791e,
    0xeb8d0e25a65e3e58,
    0xbbcbd3df14e08580,
    0xcf751f27ecdab2c,
    0x2b4da14f2613d8f4,
];

/// A generated candidate: the split position (where its 64-byte window
/// starts, the injection point) and the match offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LdmSeq {
    pub split: u64,
    pub offset: u32,
}

/// Bucketed split table plus the rolling hash state. The state is carried
/// across `fill`/`generate` calls of one frame or job; [`Self::restart`]
/// is the frame/job boundary.
pub(super) struct LdmState {
    /// Packed bucket entries: low 32 bits the biased position, high 32 the
    /// checksum. `ENTS_PER_BUCKET` entries per bucket, round-robin.
    table: Vec<u64>,
    /// Insert cursor per bucket.
    bucket_offsets: Vec<u8>,
    /// Gear rolling hash, valid exactly up to `fed`.
    rolling: u64,
    /// Split trigger; the mask's bits sit at the top of the 64-byte window.
    stop_mask: u64,
    /// Bucket index bits (`hash_log - BUCKET_SIZE_LOG`).
    hash_bits: u32,
    /// Maximum candidate offset (the row's window).
    window: u64,
    /// Absolute position up to which the rolling hash has been fed.
    fed: u64,
    /// Entry floor: the last gear (re)arm's window start, below which no
    /// split may index.
    arm: u64,
}

impl LdmState {
    /// Build a state holding one window of splits: `hash_log` entries at
    /// one split per `2^HASH_RATE_LOG` bytes covers `window` exactly
    /// (C's `ZSTD_ldm_adjustParameters` for the lazy family).
    pub fn new(window_log: u32, window: u64) -> Self {
        let hash_log = window_log
            .saturating_sub(HASH_RATE_LOG)
            .clamp(BUCKET_SIZE_LOG + 3, 20);
        let buckets = 1usize << (hash_log - BUCKET_SIZE_LOG);
        Self {
            table: alloc::vec![0u64; buckets * ENTS_PER_BUCKET],
            bucket_offsets: alloc::vec![0u8; buckets],
            rolling: !(u32::MAX as u64),
            stop_mask: ((1u64 << HASH_RATE_LOG) - 1) << (MIN_MATCH_LENGTH as u32 - HASH_RATE_LOG),
            hash_bits: hash_log - BUCKET_SIZE_LOG,
            window,
            fed: 0,
            arm: 0,
        }
    }

    /// Frame or job boundary: drop every table entry (a pooled matcher's
    /// output must not depend on earlier jobs) and re-arm the hash fresh
    /// at `pos`, the base the next `fill`/`generate` starts from (a
    /// 64-byte blind spot per boundary, C's per-chunk reset).
    pub fn restart(&mut self, pos: u64) {
        self.table.fill(0);
        self.bucket_offsets.fill(0);
        self.rolling = !(u32::MAX as u64);
        self.fed = pos;
        self.arm = pos;
    }

    /// The configured reach (the row window; table sizing key).
    pub fn window(&self) -> u64 {
        self.window
    }

    #[inline]
    fn bucket(&self, hash: usize) -> &[u64] {
        &self.table[hash * ENTS_PER_BUCKET..(hash + 1) * ENTS_PER_BUCKET]
    }

    /// Round-robin insert (C's `ZSTD_ldm_insertEntry`).
    #[inline]
    fn insert(&mut self, hash: usize, pos: u64, checksum: u32) {
        let slot = hash * ENTS_PER_BUCKET + self.bucket_offsets[hash] as usize;
        self.table[slot] = (checksum as u64) << 32 | pack_pos(pos) as u64;
        let next = self.bucket_offsets[hash] as usize + 1;
        self.bucket_offsets[hash] = (next & (ENTS_PER_BUCKET - 1)) as u8;
    }

    /// Feed the rolling hash over the 64 bytes ending at `pos` without
    /// registering splits (C's `ZSTD_ldm_gear_reset`); the window start
    /// becomes the entry floor.
    fn gear_rearm(&mut self, win: &[u8], win_base: u64, pos: u64) {
        let mut hash = self.rolling;
        let idx = (pos - MIN_MATCH_LENGTH as u64 - win_base) as usize;
        for k in 0..MIN_MATCH_LENGTH {
            hash = (hash << 1).wrapping_add(GEAR_TAB[win[idx + k] as usize]);
        }
        self.rolling = hash;
        self.fed = pos;
        self.arm = pos - MIN_MATCH_LENGTH as u64;
    }

    /// Feed `[base, end)` (absolute positions into `win`), collecting up
    /// to [`BATCH_SIZE`] split entry positions ≥ `floor`. Returns the fed
    /// byte count; a full batch stops early so its splits are inserted
    /// before the state runs ahead (C's `ZSTD_ldm_gear_feed`).
    fn gear_feed(
        &mut self,
        win: &[u8],
        win_base: u64,
        base: u64,
        end: u64,
        floor: u64,
        splits: &mut [u64; BATCH_SIZE],
    ) -> (usize, usize) {
        debug_assert_eq!(self.fed, base);
        let mut hash = self.rolling;
        let mask = self.stop_mask;
        let start = (base - win_base) as usize;
        let mut n = 0usize;
        let mut count = 0usize;
        while (base + n as u64) < end {
            hash = (hash << 1).wrapping_add(GEAR_TAB[win[start + n] as usize]);
            n += 1;
            if hash & mask == 0 {
                let trigger = base + n as u64;
                if trigger >= floor + MIN_MATCH_LENGTH as u64 {
                    splits[count] = trigger - MIN_MATCH_LENGTH as u64;
                    count += 1;
                    if count == BATCH_SIZE {
                        break;
                    }
                }
            }
        }
        self.rolling = hash;
        self.fed = base + n as u64;
        (n, count)
    }

    /// Hash the 64 bytes at `split` into (bucket, checksum).
    fn window_hash(&self, win: &[u8], win_base: u64, split: u64) -> (usize, u32) {
        let idx = (split - win_base) as usize;
        let mut h = Xxh64::new(0);
        h.write(&win[idx..idx + MIN_MATCH_LENGTH]);
        let x = h.finish();
        ((x as usize) & ((1 << self.hash_bits) - 1), (x >> 32) as u32)
    }

    /// Index every split of `[base, end)` without matching — the
    /// prefill/dictionary path (C's `ZSTD_ldm_fillHashTable`). `end` must
    /// sit at the window buffer's end (the block model), like generate.
    pub fn fill(&mut self, win: &[u8], win_base: u64, base: u64, end: u64) {
        let mut splits = [0u64; BATCH_SIZE];
        let mut pos = base;
        while pos < end {
            let floor = self.arm.max(win_base);
            let (n, count) = self.gear_feed(win, win_base, pos, end, floor, &mut splits);
            for &split in &splits[..count] {
                let (hash, checksum) = self.window_hash(win, win_base, split);
                self.insert(hash, split, checksum);
            }
            pos += n as u64;
        }
    }

    /// Generate candidates over `[base, end)`: every split is inserted,
    /// and a split whose bucket holds a ≥ [`MIN_MATCH_LENGTH`] forward
    /// match inside `window` and above `win_base` appends an [`LdmSeq`].
    /// Splits behind the generation anchor (covered by an emitted
    /// candidate) are still inserted but never matched (C's `split <
    /// anchor` rule); a candidate running past the fed range re-arms the
    /// hash at its end (C's overlapping-pattern skip). `end` must sit at
    /// the window buffer's end so forward counts stay block-bounded.
    pub fn generate(
        &mut self,
        out: &mut Vec<LdmSeq>,
        win: &[u8],
        win_base: u64,
        base: u64,
        end: u64,
    ) {
        debug_assert_eq!(end, win_base + win.len() as u64);
        // A shutoff gap (the driver's canary blocks feed nothing while the
        // latch is dead, see `ldm_generate`): the rolling hash is stale, so
        // splits found from it land misaligned. Re-arm at the block start —
        // the same reset a frame or job boundary does — so the canary
        // samples the far class faithfully.
        if self.fed < base {
            if base >= win_base + MIN_MATCH_LENGTH as u64 {
                self.gear_rearm(win, win_base, base);
            } else {
                self.fed = base;
                self.arm = base;
            }
        }
        let mut splits = [0u64; BATCH_SIZE];
        let mut pos = base;
        let mut anchor = base;
        'outer: while pos < end {
            let floor = self.arm.max(win_base);
            let (n, count) = self.gear_feed(win, win_base, pos, end, floor, &mut splits);
            let fed_end = pos + n as u64;
            for &split in &splits[..count] {
                let (hash, checksum) = self.window_hash(win, win_base, split);
                // (offset, forward match end); the longest forward run
                // wins — the consumer re-prices the offset itself.
                let mut best: Option<(u64, u64)> = None;
                if split >= anchor {
                    for e in self.bucket(hash).iter().copied() {
                        if (e >> 32) as u32 != checksum {
                            continue;
                        }
                        let dist = split.wrapping_sub(e as u32 as u64).wrapping_add(1);
                        if dist == 0 || dist > self.window {
                            continue;
                        }
                        let cand_abs = split - dist;
                        if cand_abs < win_base {
                            continue;
                        }
                        let si = (split - win_base) as usize;
                        let ci = (cand_abs - win_base) as usize;
                        let fwd = extend_match(win, si, ci);
                        if fwd < MIN_MATCH_LENGTH {
                            continue;
                        }
                        let match_end = split + fwd as u64;
                        if best.is_none_or(|(_, prev_end)| match_end > prev_end) {
                            best = Some((dist, match_end));
                        }
                    }
                }
                self.insert(hash, split, checksum);
                if let Some((dist, match_end)) = best {
                    out.push(LdmSeq {
                        split,
                        offset: dist as u32,
                    });
                    anchor = match_end;
                    if anchor > fed_end {
                        // The match overran the fed range (repeating
                        // overlap): index one repetition, skip the rest.
                        self.gear_rearm(win, win_base, anchor);
                        pos = anchor;
                        continue 'outer;
                    }
                }
            }
            pos = fed_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic random bytes (xorshift64*).
    fn rand_bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s ^= s >> 12;
                s ^= s << 25;
                s ^= s >> 27;
                (s.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u8
            })
            .collect()
    }

    /// Generate over `data` in `block`-sized chunks through one state and
    /// verify the roundtrip contract of every candidate: offset ≥ 1,
    /// within the window, source above zero, and ≥ MIN_MATCH_LENGTH
    /// agreeing bytes.
    fn collect_and_verify(data: &[u8], block: usize, window_log: u32) -> Vec<LdmSeq> {
        let mut st = LdmState::new(window_log, 1 << window_log);
        let mut seqs = Vec::new();
        let mut off = 0usize;
        while off < data.len() {
            let end = (off + block).min(data.len());
            st.generate(&mut seqs, &data[..end], 0, off as u64, end as u64);
            off = end;
        }
        for s in &seqs {
            assert!(s.offset >= 1, "offset 0");
            assert!(s.split >= s.offset as u64, "source below zero");
            assert!(
                s.offset as u64 <= 1 << window_log,
                "offset {} beyond window",
                s.offset
            );
            let fwd = extend_match(data, s.split as usize, s.split as usize - s.offset as usize);
            assert!(
                fwd >= MIN_MATCH_LENGTH,
                "candidate at {} offset {} verifies only {}",
                s.split,
                s.offset,
                fwd
            );
        }
        seqs
    }

    #[test]
    fn random_data_yields_no_candidates() {
        let data = rand_bytes(1 << 20, 7);
        assert!(collect_and_verify(&data, 128 * 1024, 21).is_empty());
    }

    #[test]
    fn far_repeat_yields_candidate() {
        // The table must retain one block's splits across a megabyte of
        // random data and emit a candidate at the far copy.
        let block = rand_bytes(8192, 0xfeed);
        let mut data = block.clone();
        data.extend(rand_bytes(1 << 20, 11));
        data.extend_from_slice(&block);
        let seqs = collect_and_verify(&data, 128 * 1024, 21);
        assert!(
            seqs.iter()
                .any(|s| s.split >= (1 << 20) && s.offset >= (1 << 20) - 8192),
            "no far candidate in {} seqs",
            seqs.len()
        );
    }

    #[test]
    fn window_bounds_candidates() {
        // A repeat beyond the 128 KiB window must not emit an offset
        // outside it, whether rejected by the reach check or eviction.
        let block = rand_bytes(8192, 0xbeef);
        let mut data = rand_bytes(64 * 1024, 22);
        data.extend_from_slice(&block);
        data.extend(rand_bytes(64 * 1024, 23));
        data.extend_from_slice(&block);
        let seqs = collect_and_verify(&data, 128 * 1024, 17);
        assert!(
            seqs.iter().all(|s| s.offset as u64 <= 1 << 17),
            "candidate beyond the window leaked"
        );
    }

    #[test]
    fn covered_splits_never_match() {
        // One full duplicate region: the first candidate's anchor covers
        // the interior splits; candidates cannot overlap.
        let half = rand_bytes(1 << 17, 0x9999);
        let mut data = half.clone();
        data.extend_from_slice(&half);
        let seqs = collect_and_verify(&data, 1 << 20, 21);
        let mut last_end = 0u64;
        for s in &seqs {
            assert!(
                s.split >= last_end,
                "candidate at {} overlaps the previous match ending at {}",
                s.split,
                last_end
            );
            last_end = last_end.max(s.split + MIN_MATCH_LENGTH as u64);
        }
    }

    #[test]
    fn fill_then_generate_finds_strip_repeat() {
        // The fill path (prefill/dictionary) indexes history no generate
        // ever fed; a repeat of that history must produce a candidate.
        let strip = rand_bytes(64 * 1024, 0x5aa5);
        let mut all = strip.clone();
        all.extend(rand_bytes(256 * 1024, 33));
        all.extend_from_slice(&strip[..4096]);
        let mut st = LdmState::new(21, 1 << 21);
        st.fill(&all, 0, 0, strip.len() as u64);
        let mut seqs = Vec::new();
        st.generate(&mut seqs, &all, 0, strip.len() as u64, all.len() as u64);
        assert!(
            seqs.iter()
                .any(|s| s.split >= (strip.len() + 256 * 1024) as u64
                    && s.offset as u64 >= 256 * 1024),
            "no candidate into the filled strip"
        );
    }

    #[test]
    fn job_restart_fills_from_strip() {
        // A pooled matcher restarts at the strip end: entries from the
        // strip survive, a repeat of it emits a candidate, and the state
        // is usable without any prior feed.
        let strip = rand_bytes(128 * 1024, 0xd1ce);
        let mut all = strip.clone();
        all.extend(rand_bytes(64 * 1024, 44));
        all.extend_from_slice(&strip[..8192]);
        let mut st = LdmState::new(21, 1 << 21);
        st.restart(0);
        st.fill(&all, 0, 0, strip.len() as u64);
        let mut seqs = Vec::new();
        st.generate(&mut seqs, &all, 0, strip.len() as u64, all.len() as u64);
        assert!(
            seqs.iter().any(
                |s| s.split >= (strip.len() + 64 * 1024) as u64 && s.offset as u64 >= 64 * 1024
            ),
            "no candidate after restart"
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let block = rand_bytes(1 << 16, 0xc0de);
        let mut data = rand_bytes(300 * 1024, 5);
        data.extend_from_slice(&block);
        data.extend_from_slice(&block);
        let a = collect_and_verify(&data, 128 * 1024, 21);
        let b = collect_and_verify(&data, 128 * 1024, 21);
        assert_eq!(a, b);
    }

    #[test]
    fn tiny_inputs_are_inert() {
        let data = rand_bytes(63, 1);
        let mut st = LdmState::new(10, 1 << 10);
        let mut seqs = Vec::new();
        st.generate(&mut seqs, &data, 0, 0, data.len() as u64);
        assert!(seqs.is_empty());
        // The carried state continues seamlessly into the next block.
        let mut all = data;
        all.extend(rand_bytes(4096, 2));
        st.generate(&mut seqs, &all, 0, 63, all.len() as u64);
        assert!(seqs.is_empty());
    }

    #[test]
    fn split_window_at_region_tail_stays_in_bounds() {
        // Splits triggered by the final bytes of the fed region: their
        // 64-byte window reads must stay inside the buffer.
        let block = rand_bytes(4096, 0x77);
        let mut data = rand_bytes(1 << 16, 9);
        data.extend_from_slice(&block);
        data.extend_from_slice(&block);
        assert!(!collect_and_verify(&data, data.len(), 17).is_empty());
    }
}
