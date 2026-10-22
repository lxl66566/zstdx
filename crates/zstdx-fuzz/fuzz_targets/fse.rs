#![no_main]
//! FSE codec robustness: the generic round trip, plus the R64 NCount parity
//! classes — arbitrary bytes as NCount descriptions under every sequence
//! table's (max symbol, max log) cap with truncated/shifted prefixes, and
//! constructed distributions (packed 0xFFFF zero-run markers, full 255-symbol
//! and over-full 257-symbol alphabets, near-max-entropy Kraft-exact tables)
//! that must serialize and parse back byte-exactly.

use libfuzzer_sys::fuzz_target;
use zstdx::fse::{FSETable, round_trip, serialize_ncount};

fuzz_target!(|data: &[u8]| {
    round_trip(data);
    ncount_malformed(data);
    ncount_constructed(data);
});

/// Parse arbitrary bytes (and truncated / zero-prefixed variants) as NCount
/// streams under each sequence-table cap. Any outcome is fine as long as the
/// parser fails cleanly.
fn ncount_malformed(data: &[u8]) {
    let variants: Vec<&[u8]> = if data.is_empty() {
        vec![data]
    } else {
        let mut v = vec![data, &data[..data.len() - 1], &data[..data.len() / 2]];
        if data.len() > 1 {
            v.push(&data[1..]);
        }
        v
    };
    for &(max_symbol, max_log) in &[(255u8, 12u8), (52, 9), (35, 9), (31, 8)] {
        for variant in &variants {
            let mut table = FSETable::new(max_symbol);
            let _ = table.build_decoder(variant, max_log);
        }
    }
    let mut padded = Vec::with_capacity(data.len() + 1);
    padded.extend_from_slice(data);
    padded.push(0);
    let mut table = FSETable::new(255);
    let _ = table.build_decoder(&padded, 12);
}

/// Constructed distributions covering the serializer/parser pair's edge
/// shapes; every Kraft-exact one must round-trip with the parser consuming
/// exactly the written bytes, and an alphabet over the cap must be rejected.
fn ncount_constructed(data: &[u8]) {
    // Packed 0xFFFF zero-run markers: live symbols separated by dead runs
    // longer than 24 force the serializer's packed marker form.
    let mut probs = vec![0i32; 64];
    probs[0] = 16;
    probs[30] = 8;
    probs[60] = 8;
    assert_kraft_exact_roundtrip(&probs, 5, 255, data);

    // Near-max-entropy Kraft-exact table: a full 255-symbol alphabet over
    // tableLog 12 with data-driven weight bumps.
    let mut probs = vec![16i32; 255];
    let mut sum: i32 = 16 * 255;
    let mut i = 0;
    while sum < (1 << 12) {
        let add = 1 + (data.get(i).copied().unwrap_or(0) & 0x3f) as i32;
        let idx = data.get(i).copied().unwrap_or(i as u8) as usize % 255;
        probs[idx] += add;
        sum += add;
        i += 1;
    }
    let mut over = sum - (1 << 12);
    let mut idx = 254;
    while over > 0 {
        let take = over.min(probs[idx] - 1);
        probs[idx] -= take;
        over -= take;
        idx -= 1;
    }
    assert_kraft_exact_roundtrip(&probs, 12, 255, data);

    // An alphabet past the 255-symbol cap parses as TooManySymbols
    // (libzstd's FSE_readNCount: charnum >= maxSV1 break, then the remaining
    // != 1 corruption). The first 256 symbols must still be short of the sum
    // when they are done, so the reader cannot stop at the cap: one state
    // each, symbol 256 carries the balance.
    let mut overfull = vec![1i32; 257];
    overfull[256] = (1 << 12) - 256;
    let bytes = serialize_ncount(&overfull, 12);
    let mut table = FSETable::new(255);
    assert!(table.build_decoder(&bytes, 12).is_err());
    // note: an over-subscribed distribution instead stops silently at the
    // exact sum in BOTH implementations (C breaks on remaining <= 1 with
    // remaining == 1) — the unread tail stays for the next section, so that
    // shape is a byte-accounting question, not a rejection.
    let bytes = serialize_ncount(&probs, 12);
    let mut table = FSETable::new(255);
    let used = table.build_decoder(&bytes, 12).expect("255 symbols parse");
    assert_eq!(used, bytes.len());
}

/// Serialize `probs` at `acc_log`, parse it back under `max_symbol`, and
/// require exact byte consumption. Truncations of the same stream must not
/// panic.
fn assert_kraft_exact_roundtrip(probs: &[i32], acc_log: u8, max_symbol: u8, data: &[u8]) {
    let bytes = serialize_ncount(probs, acc_log);
    let mut table = FSETable::new(max_symbol);
    let used = table
        .build_decoder(&bytes, 12)
        .expect("serializer output must parse");
    assert_eq!(used, bytes.len());
    // truncations at both edges must fail cleanly
    let mut t = FSETable::new(max_symbol);
    let _ = t.build_decoder(&bytes[..bytes.len() - 1], 12);
    let mut t = FSETable::new(max_symbol);
    let _ = t.build_decoder(&bytes[1..], 12);
    // data-seeded single-bit flips of the stream: parse must not panic
    for i in 0..bytes.len().min(8) {
        let mut flipped = bytes.clone();
        flipped[i] ^= 1 + (data.get(i).copied().unwrap_or(0) & 0x7f);
        let mut t = FSETable::new(max_symbol);
        let _ = t.build_decoder(&flipped, 12);
    }
}
