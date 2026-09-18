use alloc::vec::Vec;

use super::{LdmArming, MatchGeneratorDriver, ldm_min_window, pack_pos, unpack_pos};
use crate::encoding::{Matcher, Sequence};

/// The 64 KiB unit both LDM far-class tests duplicate.
const UNIT: usize = 64 * 1024;

/// The shared prefix fill (the streaming finish tail's strip-fill
/// share) must reproduce the stock per-job strip fill bit for bit: a
/// driver that adopts a snapshot of a shorter prefix fill and
/// continues it, and a builder that continues its own fill, both end
/// with exactly the tables a from-scratch `prefill_job_strip` of the
/// same strip produced.
#[cfg(feature = "std")]
#[test]
fn strip_snapshot_adopt_is_exact() {
    // Far repeats (a block duplicated megabytes apart) so the LDM
    // split pass and the stride-3 grid both have entries to evolve.
    let mut s = 0x1234_5678_9abc_def0u64;
    let mut rand = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let head: Vec<u8> = (0..512 * 1024).map(|_| (rand() & 0xff) as u8).collect();
    let mut data = head.clone();
    for _ in 0..(1024 * 1024 / 8) {
        data.extend_from_slice(&rand().to_le_bytes());
    }
    data.extend_from_slice(&head);
    while data.len() < 4 * 1024 * 1024 {
        data.extend_from_slice(&head[..data.len().min(1024)]);
    }
    // An unaligned final strip end exercises the tail loop both paths
    // share; the segment boundaries stay grid-aligned (multiples of
    // four) as `build_spf` keeps them.
    data.truncate(data.len() - 5);
    let (seg, med) = (1024 * 1024, 2 * 1024 * 1024);

    let fresh = || {
        let mut d = MatchGeneratorDriver::new_direct();
        d.set_ldm_arming(LdmArming::Job);
        d.reset(crate::Level::Balanced);
        d
    };
    let mut stock = fresh();
    stock.prefill_job_strip(&data, 0);

    let mut builder = fresh();
    builder.prefill_window(&data[..0], 0);
    let mut from = 0u64;
    let mut upto;
    loop {
        let soft = (from + seg as u64).min(med as u64);
        upto = builder
            .strip_fill_segment(&data, 0, from, soft)
            .expect("a freeze lands within each segment's slack");
        from = upto;
        if upto >= med as u64 {
            break;
        }
    }
    let snap = builder.snapshot_strip_fill(upto);
    builder.strip_fill_continue(&data, 0, upto);

    let mut adopted = fresh();
    adopted.adopt_strip_snapshot(&snap, &data, 0);

    assert_eq!(stock.tables, adopted.tables, "head grid tables diverge");
    assert_eq!(stock.second, adopted.second, "chain split diverges");
    assert_eq!(stock.tables, builder.tables, "builder head grid diverges");
    assert_eq!(stock.second, builder.second, "chain split diverges");
    assert_eq!(
        stock.seed_offset, adopted.seed_offset,
        "seed offset diverges"
    );
    let (s_ldm, a_ldm, b_ldm) = (
        stock.ldm.as_ref().unwrap(),
        adopted.ldm.as_ref().unwrap(),
        builder.ldm.as_ref().unwrap(),
    );
    assert!(
        s_ldm.snapshot().same_as(&a_ldm.snapshot()),
        "adopted LDM diverges"
    );
    assert!(
        s_ldm.snapshot().same_as(&b_ldm.snapshot()),
        "builder LDM diverges"
    );
}

#[test]
fn ldm_arming_bars() {
    // The mid-size bar arms one step under the row window; the job bar
    // requires the clamp to have left the row window intact; the probe's
    // keep parse and the far-dead screen never arm.
    assert_eq!(ldm_min_window(LdmArming::Frame), Some(1 << 25));
    #[cfg(feature = "std")]
    {
        assert_eq!(ldm_min_window(LdmArming::Job), Some(1 << 26));
        assert_eq!(ldm_min_window(LdmArming::FarDead), None);
    }
    assert_eq!(ldm_min_window(LdmArming::ProbeKeep), None);
}

fn block_label(i: usize) -> Vec<u8> {
    // "block N filler text; " without needing format! in no_std tests
    let mut v = Vec::new();
    v.extend_from_slice(b"block ");
    v.push(b'0' + i as u8);
    v.extend_from_slice(b" filler text; ");
    v
}

/// [`match_and_reconstruct`] at the default block size, recording every
/// emitted sequence's offset.
fn match_and_reconstruct_collecting_offsets(data: &[u8], offsets: &mut Vec<usize>) -> Vec<u8> {
    let mut driver = MatchGeneratorDriver::new(128 * 1024);
    driver.reset(crate::Level::Balanced);
    let mut rep = [1u32, 4, 8];
    let mut reconstructed = Vec::new();
    for block in data.chunks(128 * 1024) {
        driver.block_tail()[..block.len()].copy_from_slice(block);
        driver.commit_block(block.len());
        driver.start_matching(|seq| match seq {
            Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
            Sequence::Triple {
                literals,
                offset,
                match_len,
            } => {
                reconstructed.extend_from_slice(literals);
                offsets.push(offset);
                let actual = crate::decoding::sequence_execution::do_offset_history(
                    offset as u32,
                    literals.len() as u32,
                    &mut rep,
                );
                let start = reconstructed.len() - actual as usize;
                for i in 0..match_len {
                    let b = reconstructed[start + i];
                    reconstructed.push(b);
                }
            },
        });
    }
    reconstructed
}

/// Feed `data` through the matcher one block at a time and reconstruct the
/// original from the emitted sequences.
fn match_and_reconstruct(data: &[u8], block_size: usize) -> Vec<u8> {
    let mut driver = MatchGeneratorDriver::new(block_size);
    driver.reset(crate::Level::Fastest);
    // Offset history mirrors the decoder's per-frame scratch.
    let mut rep = [1u32, 4, 8];
    let mut reconstructed = Vec::new();
    for block in data.chunks(block_size) {
        driver.block_tail()[..block.len()].copy_from_slice(block);
        driver.commit_block(block.len());
        driver.start_matching(|seq| match seq {
            Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
            Sequence::Triple {
                literals,
                offset,
                match_len,
            } => {
                reconstructed.extend_from_slice(literals);
                let actual = crate::decoding::sequence_execution::do_offset_history(
                    offset as u32,
                    literals.len() as u32,
                    &mut rep,
                );
                // Matches may overlap their own output (offset < match_len).
                let start = reconstructed.len() - actual as usize;
                for i in 0..match_len {
                    let b = reconstructed[start + i];
                    reconstructed.push(b);
                }
            },
        });
    }
    reconstructed
}

#[test]
fn reconstructs_short_runs() {
    let mut data = Vec::new();
    data.extend([0u8; 16]);
    data.extend([1u8, 2, 3, 4, 5, 6]);
    data.extend([1u8, 2, 3, 4, 5, 6]);
    data.extend([0u8; 8]);
    assert_eq!(match_and_reconstruct(&data, 8), data);
    assert_eq!(match_and_reconstruct(&data, 4), data);
}

#[test]
fn reconstructs_tiny_rep_inputs() {
    // Fuzz-found (2026-09-14): an 8-byte input whose rep probe hits at
    // position 1 gives insert_covered a match start past insert_max
    // (win_base + 0 here); the wrapped `end - p` parity peel then
    // hashed 8 bytes past the input. Guarded empty-range now.
    for data in [
        &[0x0fu8; 6][..],
        &[0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x2a, 0xff][..],
        &[0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x2a, 0x90][..],
    ] {
        assert_eq!(&match_and_reconstruct(data, 8)[..], data);
    }
}

#[test]
fn reconstructs_across_blocks() {
    // Matches must reach into previous blocks through the shared window.
    let mut data = Vec::new();
    for i in 0..10 {
        data.extend_from_slice(&[0xa5, 0x5a, 0xc3, 0x3c, 0x99, 0x66, 0xf0, 0x0f]);
        data.extend_from_slice(&block_label(i));
    }
    assert_eq!(match_and_reconstruct(&data, 32), data);
    assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
}

#[test]
fn reconstructs_random() {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut data = Vec::with_capacity(300 * 1024);
    while data.len() < 300 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(&state.to_le_bytes());
    }
    assert_eq!(match_and_reconstruct(&data, 64 * 1024), data);
}

#[test]
fn reconstructs_after_compaction() {
    // 3 MiB of data with repeats forces window compaction to run.
    let mut data = Vec::with_capacity(3 << 20);
    for i in 0..(3 << 20) / 64 {
        data.push((i % 251) as u8);
        data.extend(&[7u8; 63]);
    }
    assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
}

/// Far repeats beyond the chain reach must ride LDM candidates: two
/// copies of a random block 5 MiB apart inside filler, compressed at
/// the balanced row (window W26, chain reach W22). The parse is only
/// legal if some sequence references the far class.
#[test]
fn ldm_row_covers_beyond_chain_reach() {
    let mut state = 0x0123_4567_89ab_cdefu64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // dll-shaped: 80 distinct 64 KiB blocks, then a run of far copies
    // (5 MiB back, beyond the row's 4 MiB chain reach) spaced 64 KiB
    // apart — the far class recurs throughout, like shared code in
    // concatenated binaries, instead of one isolated duplicate (which
    // the quiet latch could legitimately miss; see LDM_QUIET).
    let mut data = Vec::with_capacity(6 * 1024 * 1024);
    for _ in 0..80 {
        data.extend((0..UNIT).map(|_| (rand() >> 32) as u8));
    }
    for i in 0..16 {
        let unit: Vec<u8> = data[i * UNIT..(i + 1) * UNIT].to_vec();
        data.extend_from_slice(&unit);
    }
    let mut offsets = Vec::new();
    let reconstructed = match_and_reconstruct_collecting_offsets(&data, &mut offsets);
    assert_eq!(reconstructed, data);
    assert!(
        offsets.iter().any(|&o| o > (1 << 22)),
        "no sequence beyond the chain reach: LDM far class missing"
    );
}

/// dll corpus roundtrips through the balanced row's LDM path, when the
/// generated large-binary corpus is present (gitignored; built by
/// bench/gen_big.sh): dll100 exercises the full W26 reach, dll32 the
/// source-clamped window.
#[test]
#[cfg(feature = "std")]
fn ldm_dll_roundtrip() {
    for name in ["bench/big/dll100.raw", "bench/big/dll32.raw"] {
        let Ok(raw) = std::fs::read(name) else {
            continue;
        };
        let c = crate::encoding::compress_slice_opts(&raw, crate::Level::Balanced, false);
        let d = crate::bulk::decompress(&c, raw.len()).expect("decode");
        assert_eq!(d, raw, "{name}");
    }
}

/// Far repeats beyond the tree domain must ride LDM candidates on the
/// opt and btlazy2 rows too (wide W26 frame window, tree domain at the
/// stock row window): the same shape as the balanced test above, at
/// Best, Opt and Ultra, with the copies 12 MiB apart — beyond the
/// rows' 4-8 MiB domains.
#[test]
fn ldm_high_rows_cover_beyond_tree_domain() {
    let mut state = 0x0123_4567_89ab_cdefu64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut data = Vec::with_capacity(14 * 1024 * 1024);
    for _ in 0..200 {
        data.extend((0..UNIT).map(|_| (rand() >> 32) as u8));
    }
    for i in 0..16 {
        let unit: Vec<u8> = data[i * UNIT..(i + 1) * UNIT].to_vec();
        data.extend_from_slice(&unit);
    }
    for level in [crate::Level::Best, crate::Level::Opt, crate::Level::Ultra] {
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(level);
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        let mut far = false;
        for block in data.chunks(128 * 1024) {
            driver.block_tail()[..block.len()].copy_from_slice(block);
            driver.commit_block(block.len());
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    if offset > (1 << 23) {
                        far = true;
                    }
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    let start = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[start + i];
                        reconstructed.push(b);
                    }
                },
            });
        }
        assert_eq!(reconstructed, data, "reconstruct {level:?}");
        assert!(far, "{level:?}: no sequence beyond the tree domain");
    }
}

#[test]
fn opt_levels_reconstruct() {
    let mut data = Vec::with_capacity(700 * 1024);
    let words = [
        &b"the quick brown fox "[..],
        &b"jumps over the lazy dog "[..],
        &b"lorem ipsum dolor sit amet "[..],
        b"\x00\x01\x02\x03 structured noise ",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    while data.len() < 700 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(words[(state as usize) % words.len()]);
    }
    for level in [crate::Level::Best, crate::Level::Opt, crate::Level::Ultra] {
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(level);
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for block in data.chunks(128 * 1024) {
            driver.block_tail()[..block.len()].copy_from_slice(block);
            driver.commit_block(block.len());
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    let start = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[start + i];
                        reconstructed.push(b);
                    }
                },
            });
        }
        assert_eq!(reconstructed, data, "reconstruct {level:?}");
    }
}

#[test]
fn pos_entry_roundtrip_across_cycles() {
    // A live entry always resolves back to its position, even across a
    // 4 GiB cycle boundary; a value numerically above the scan position
    // with no cycle to unwrap into is dead (stale cross-frame entry).
    for abs in [0u64, 1, 7, 0xffff_fffe, 5_000_000_000, 1 << 40] {
        for delta in [1u64, 2, 0x123, 0xffff_f000] {
            let pos = abs + delta;
            assert_eq!(unpack_pos(pack_pos(abs), pos), Some(abs), "{abs}+{delta}");
        }
    }
    // 2^32 - 1 collides with the empty sentinel: one dead position per
    // 4 GiB cycle, by design.
    assert_eq!(unpack_pos(pack_pos(0xffff_ffff), 1 << 40), None);
    assert_eq!(unpack_pos(pack_pos(1000), 10), None);
    assert_eq!(unpack_pos(0, 1 << 40), None);
}

/// The seed scan must return exactly the nearest qualifying position a
/// naive byte-wise walk would find, on every path: the AVX-512 block
/// walk (residue classes mod 8, block boundaries, the anchor's trivial
/// self-match), the scalar tail below the last block, and strips too
/// short for the block scheme.
#[test]
fn seed_scan_matches_naive_walk() {
    let naive = |data: &[u8], last: usize| -> Option<usize> {
        let a8 = super::read8(data, last);
        (0..last).rev().find(|&u| {
            super::read8(data, u) == a8
                && super::seed_agrees(data, u, last)
                && super::seed_confirms(data, u, last)
        })
    };
    let check = |data: &[u8]| {
        let last = data.len() - 8;
        let expect = naive(data, last);
        assert_eq!(
            super::seed_scan(data, last, super::read8(data, last)),
            expect,
            "dispatched scan at len {}",
            data.len()
        );
        #[cfg(all(target_arch = "x86_64", feature = "std"))]
        if last >= super::SEED_SCAN_MIN && std::is_x86_feature_detected!("avx512f") {
            assert_eq!(
                // SAFETY: feature detected above; bounds argued at the
                // definition.
                unsafe { super::seed_scan_avx512(data, last, super::read8(data, last)) },
                expect,
                "avx512 scan at len {}",
                data.len()
            );
        }
    };

    let mut state = 0x0123_4567_89ab_cdefu64;
    let mut rng = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // A 56-byte pattern planted as the anchor window `[last-48,
    // last+8)` and, disjoint from it, as the candidate window
    // `[hit-48, hit+8)`; everything else random. The candidate is a
    // confirmed repeat: the u64s behind it at each confirm offset
    // (where it has history) mirror the anchor's. `hit` sweeps the
    // interesting offsets: residue classes mod 8, the block-grid
    // boundaries and the bottom tail (< 64).
    let len = 700;
    let last = len - 8;
    let mut data = alloc::vec![0u8; len];
    for hit in [
        last - 56,
        last - 57,
        last - 63,
        last - 64,
        last - 65,
        last - 72,
        last - 119,
        last - 120,
        last - 128,
        65,
        64,
        63,
        48,
    ] {
        for b in &mut data {
            *b = rng() as u8;
        }
        let pat: Vec<u8> = (0..56).map(|_| rng() as u8).collect();
        data[hit - 48..hit + 8].copy_from_slice(&pat);
        data[last - 48..].copy_from_slice(&pat);
        for t in super::SEED_CONFIRMS {
            if hit >= t {
                let a = super::read8(&data, last - t);
                data[hit - t..hit - t + 8].copy_from_slice(&a.to_le_bytes());
            }
        }
        check(&data);
        assert_eq!(
            super::seed_scan(&data, last, super::read8(&data, last)),
            Some(hit),
            "planted hit {hit}"
        );
    }
    // A candidate below the agree bound never qualifies: the planted
    // anchor window alone must yield no seed.
    for b in &mut data {
        *b = rng() as u8;
    }
    let pat: Vec<u8> = (0..56).map(|_| rng() as u8).collect();
    data[last - 48..].copy_from_slice(&pat);
    check(&data);
    assert_eq!(
        super::seed_scan(&data, last, super::read8(&data, last)),
        None
    );
    // A local block repeat passes the 56-byte window but not the spread
    // confirms; only a confirmed candidate may become the seed, so the
    // nearer unconfirmed plant is skipped for the farther confirmed one
    // — and with no confirmed plant at all there is no seed.
    for far in [Some(last - 300), None] {
        for b in &mut data {
            *b = rng() as u8;
        }
        data[last - 48..].copy_from_slice(&pat);
        data[last - 148..last - 92].copy_from_slice(&pat); // near, unconfirmed
        if let Some(hit) = far {
            data[hit - 48..hit + 8].copy_from_slice(&pat);
            for t in super::SEED_CONFIRMS {
                if hit >= t {
                    let a = super::read8(&data, last - t);
                    data[hit - t..hit - t + 8].copy_from_slice(&a.to_le_bytes());
                }
            }
        }
        check(&data);
        assert_eq!(
            super::seed_scan(&data, last, super::read8(&data, last)),
            far,
            "confirmed candidate {far:?}"
        );
    }
    // All-same bytes: every position trivially matches, nearest wins and
    // the anchor itself is excluded.
    let flat = alloc::vec![0x5Au8; len];
    check(&flat);
    assert_eq!(
        super::seed_scan(&flat, last, super::read8(&flat, last)),
        Some(last - 1)
    );
    // Exact periods across all residue classes mod 8: the scan must find
    // the period (or a multiple) exactly like the naive walk.
    for period in [200, 201, 202, 203, 204, 205, 206, 207, 256, 257] {
        let unit: Vec<u8> = (0..period).map(|_| rng() as u8).collect();
        let mut tiled = alloc::vec![0u8; 0];
        while tiled.len() < len {
            tiled.extend_from_slice(&unit);
        }
        tiled.truncate(len);
        check(&tiled);
    }
    // No repeat at all, at sizes just around the block-scheme minimum.
    for l in [48, 56, 63, 64, 120, 127, 128, 129, 136, len] {
        let mut data = alloc::vec![0u8; l];
        for b in &mut data {
            *b = rng() as u8;
        }
        // A u64-rng strip of this size has no 8-byte recurrence.
        check(&data);
    }
}

/// Stale u32 entries from an earlier frame must die on the domain
/// check: their values share no position domain with the new frame's
/// scan, and the wrap reconstruction must reject the ones numerically
/// above it instead of underflowing into an out-of-bounds window index.
/// Regression for a pooled-driver double-frame compression.
#[test]
fn stale_entries_survive_frame_reset() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rng = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // Frame 1: > 1 MiB so the tables end holding positions far above
    // frame 2's scan. Frame 2: small and structured, so its probes hit
    // stale slots from the first position on.
    let mut long = Vec::with_capacity(3 << 20);
    while long.len() < 3 << 20 {
        long.extend_from_slice(&rng().to_le_bytes());
        long.extend_from_slice(b"stable filler segment; ");
    }
    let mut small = Vec::new();
    for i in 0..64 {
        small.extend_from_slice(&block_label(i % 7));
        small.push(rng() as u8);
    }
    for level in [
        crate::Level::Fastest,
        crate::Level::Fast,
        crate::Level::Balanced,
    ] {
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        for data in [&long, &small] {
            driver.reset(level);
            let mut rep = [1u32, 4, 8];
            let mut reconstructed = Vec::new();
            for block in data.chunks(128 * 1024) {
                driver.block_tail()[..block.len()].copy_from_slice(block);
                driver.commit_block(block.len());
                driver.start_matching(|seq| match seq {
                    Sequence::Literals { literals } => {
                        reconstructed.extend_from_slice(literals);
                    },
                    Sequence::Triple {
                        literals,
                        offset,
                        match_len,
                    } => {
                        reconstructed.extend_from_slice(literals);
                        let actual = crate::decoding::sequence_execution::do_offset_history(
                            offset as u32,
                            literals.len() as u32,
                            &mut rep,
                        );
                        let start = reconstructed.len() - actual as usize;
                        for i in 0..match_len {
                            let b = reconstructed[start + i];
                            reconstructed.push(b);
                        }
                    },
                });
            }
            assert_eq!(reconstructed, *data, "reconstruct {level:?}");
        }
    }
}

#[test]
fn skip_matching_indexes_for_later_blocks() {
    let mut driver = MatchGeneratorDriver::new(16);
    driver.reset(crate::Level::Fastest);
    let pattern = [3u8, 1, 4, 1, 5, 9, 2, 6];
    driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
    driver.commit_block(pattern.len());
    driver.skip_matching();
    driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
    driver.commit_block(pattern.len());
    let mut got_triple = false;
    driver.start_matching(|seq| {
        if let Sequence::Triple { offset, .. } = seq {
            // New-offset wire value: actual offset 8 encodes as 8 + 3.
            assert_eq!(offset, pattern.len() + 3);
            got_triple = true;
        }
    });
    assert!(
        got_triple,
        "second block must match the skipped first block"
    );
}

#[test]
fn gated_block_stays_match_history() {
    // A near-random block big enough for the incompressibility gate must
    // gate; a later duplicate must NOT gate (probe hit) and must match
    // into the gated block — through the catch-up fill for the table
    // strategies, the lazy tree fill for the opt strategies.
    let mut state = 0x0123_4567_89ab_cdefu64;
    let mut rng = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut block = Vec::with_capacity(20_000);
    while block.len() < 20_000 {
        block.extend_from_slice(&rng().to_le_bytes());
    }
    for level in [
        crate::Level::Fastest,
        crate::Level::Fast,
        crate::Level::Balanced,
        crate::Level::Best,
    ] {
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(level);
        driver.block_tail()[..block.len()].copy_from_slice(&block);
        driver.commit_block(block.len());
        assert!(
            driver.skip_if_incompressible(),
            "first block must gate at {level:?}"
        );
        driver.block_tail()[..block.len()].copy_from_slice(&block);
        driver.commit_block(block.len());
        assert!(
            !driver.skip_if_incompressible(),
            "duplicate must stay matchable at {level:?}"
        );
        let mut got_triple = false;
        driver.start_matching(|seq| {
            if let Sequence::Triple {
                offset, match_len, ..
            } = seq
                && !got_triple
            {
                // New-offset wire value: the actual offset is the block
                // length, encoded as len + 3.
                assert_eq!(offset, block.len() + 3, "first match offset at level");
                assert!(match_len > 1000, "duplicate must match wholesale");
                got_triple = true;
            }
        });
        assert!(
            got_triple,
            "duplicate must match the gated block at {level:?}"
        );
    }
}

#[test]
fn emits_repcode_sequences() {
    // Structured repetition at a fixed distance: the first repeat is found
    // by the hash probe (offset becomes rep[0]), later repeats must be
    // emitted as repcode 1 (wire offset value 1).
    let pattern: &[u8] = &[
        0xa5, 0x5a, 0xc3, 0x3c, 0x99, 0x66, 0xf0, 0x0d, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        0x88, 0x91, 0x82, 0x73, 0x64,
    ];
    let mut data = Vec::new();
    for i in 0..40 {
        data.extend_from_slice(pattern);
        // Constant-length, varying separators keep one pending literal in
        // front of each repeat and hold the period stable.
        data.push(0xf0 ^ i as u8);
    }
    let mut driver = MatchGeneratorDriver::new(128 * 1024);
    driver.reset(crate::Level::Fastest);
    driver.block_tail()[..data.len()].copy_from_slice(&data);
    driver.commit_block(data.len());
    let mut repcodes = 0usize;
    driver.start_matching(|seq| {
        if let Sequence::Triple { offset, .. } = seq
            && offset <= 3
        {
            repcodes += 1;
        }
    });
    assert!(
        repcodes > 0,
        "repeated structure must produce repcode matches"
    );
    assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
}

/// A multithreaded job's first parsed block runs with adopted history
/// below it (`prefill_window` + `adopt_window` + `set_block`) and the
/// repcode gate armed; Ultra's strip-tail statistics seeding must not
/// corrupt the emitted stream or the offset history.
#[test]
fn ultra_job_boundary_reconstructs() {
    let mut data = Vec::with_capacity(300 * 1024);
    let words = [
        &b"the quick brown fox "[..],
        &b"jumps over the lazy dog "[..],
        &b"lorem ipsum dolor sit amet "[..],
        b"\x00\x01\x02\x03 structured noise ",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    while data.len() < 300 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(words[(state as usize) % words.len()]);
    }
    let start = 192 * 1024;
    let run = || {
        let mut driver = MatchGeneratorDriver::new_direct();
        driver.reset(crate::Level::Ultra);
        driver.gate_repcodes();
        driver.prefill_window(&data[..start], 0);
        driver.adopt_window(&data, 0);
        driver.set_block(start as u64, data.len() as u64);
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = data[..start].to_vec();
        driver.start_matching(|seq| match seq {
            Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
            Sequence::Triple {
                literals,
                offset,
                match_len,
            } => {
                reconstructed.extend_from_slice(literals);
                let actual = crate::decoding::sequence_execution::do_offset_history(
                    offset as u32,
                    literals.len() as u32,
                    &mut rep,
                );
                let from = reconstructed.len() - actual as usize;
                for i in 0..match_len {
                    let b = reconstructed[from + i];
                    reconstructed.push(b);
                }
            },
        });
        reconstructed
    };
    assert_eq!(run(), data);
    assert_eq!(run(), data, "job parse must be deterministic");
}

/// The cold-start DUBT head (row 9) must roundtrip a frame that spans
/// the head, the handoff's dense re-index, and the chain tail — on a
/// wide alphabet (head runs) and on a small one (the alphabet gate
/// keeps the chain pure) — and stay deterministic.
#[test]
fn dubt_head_roundtrips() {
    let block = 128 * 1024;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // Wide alphabet: text-like words over > HEAD_SYMS_MIN symbols, with
    // a repeated sentence to give the head real matches.
    let mut wide = Vec::with_capacity(5 << 20);
    let sentence = b"the cold-start head parses this sentence over and over; ";
    while wide.len() < 5 << 20 {
        wide.extend_from_slice(sentence);
        for _ in 0..8 {
            wide.push(b' ' + (rand() % 64) as u8);
        }
    }
    // Narrow alphabet: 16 symbols, above HEAD_MIN_TOTAL so only the
    // alphabet gate can keep the chain pure.
    let narrow: Vec<u8> = (0..5 << 20).map(|_| (rand() & 15) as u8).collect();
    for (data, expect_head) in [(&wide[..], true), (&narrow[..], false)] {
        let run = || {
            let mut driver = MatchGeneratorDriver::new(block);
            driver.reset(crate::Level::from_zstd(9));
            let mut rep = [1u32, 4, 8];
            let mut reconstructed = Vec::new();
            for chunk in data.chunks(block) {
                driver.block_tail()[..chunk.len()].copy_from_slice(chunk);
                driver.commit_block(chunk.len());
                driver.start_matching(|seq| match seq {
                    Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                    Sequence::Triple {
                        literals,
                        offset,
                        match_len,
                    } => {
                        reconstructed.extend_from_slice(literals);
                        let actual = crate::decoding::sequence_execution::do_offset_history(
                            offset as u32,
                            literals.len() as u32,
                            &mut rep,
                        );
                        let from = reconstructed.len() - actual as usize;
                        for i in 0..match_len {
                            let b = reconstructed[from + i];
                            reconstructed.push(b);
                        }
                    },
                });
            }
            (reconstructed, driver.dubt_head)
        };
        let (first, phase) = run();
        assert_eq!(
            first, *data,
            "roundtrip failed (head expected {expect_head})"
        );
        assert_eq!(
            phase,
            if expect_head {
                super::HeadPhase::Done
            } else {
                super::HeadPhase::Off
            },
            "head lifecycle (head expected {expect_head})"
        );
        assert_eq!(run().0, *data, "parse must be deterministic");
    }
}

/// A gated (incompressible) block crossing `HEAD_LIMIT` must still
/// hand the head off to the chain: the handoff rides the first
/// dispatched block past the span, and the head region is
/// dense-indexed before the chain probes.
#[test]
fn dubt_head_hands_off_across_gated_block() {
    let block = 128 * 1024;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut text = Vec::new();
    let sentence = b"the gated block must not swallow the handoff; ";
    while text.len() < block * 8 {
        text.extend_from_slice(sentence);
        // Wide-alphabet filler so the head's alphabet gate accepts.
        for _ in 0..sentence.len() {
            text.push(b' ' + (rand() % 64) as u8);
        }
    }
    text.truncate(block * 8);
    let mut random_block = Vec::with_capacity(block);
    for _ in 0..block {
        random_block.push((rand() >> 32) as u8);
    }
    let mut data = text.clone();
    data.extend_from_slice(&random_block);
    // The tail repeats the head region: post-handoff chain blocks must
    // find matches reaching back into it.
    data.extend_from_slice(&text);
    data.extend_from_slice(&text);

    let mut driver = MatchGeneratorDriver::new(block);
    driver.reset(crate::Level::from_zstd(9));
    let mut rep = [1u32, 4, 8];
    let mut reconstructed = Vec::new();
    for chunk in data.chunks(block) {
        driver.block_tail()[..chunk.len()].copy_from_slice(chunk);
        driver.commit_block(chunk.len());
        if driver.skip_if_incompressible() {
            assert!(
                reconstructed.len() >= block * 8 && reconstructed.len() < block * 9 + block,
                "only the random block may gate"
            );
            reconstructed.extend_from_slice(chunk);
            continue;
        }
        driver.start_matching(|seq| match seq {
            Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
            Sequence::Triple {
                literals,
                offset,
                match_len,
            } => {
                reconstructed.extend_from_slice(literals);
                let actual = crate::decoding::sequence_execution::do_offset_history(
                    offset as u32,
                    literals.len() as u32,
                    &mut rep,
                );
                let from = reconstructed.len() - actual as usize;
                for i in 0..match_len {
                    let b = reconstructed[from + i];
                    reconstructed.push(b);
                }
            },
        });
    }
    assert_eq!(reconstructed, data);
    assert_eq!(driver.dubt_head, super::HeadPhase::Done);
}
