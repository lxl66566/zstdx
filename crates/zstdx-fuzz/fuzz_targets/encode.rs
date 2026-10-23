#![no_main]
//! Encoder roundtrip across the whole numeric ladder and option surface:
//! the first input byte picks the level (every strategy row from the raw
//! block to 22, so the tagged rows, the chain band and the wide rows 20-22
//! get coverage, not just the named tiers), the second byte's bits switch
//! the checksum flag, an exact pledged size, a forced window log (sub-block
//! windows included — offsets must stay inside whatever window the frame
//! declares) and the LDM override. The frame must decode back to the input
//! through both our decoder and the reference implementation.

use libfuzzer_sys::fuzz_target;
use zstdx::decoding::FrameDecoder;
use zstdx::Level;

fuzz_target!(|data: &[u8]| {
    let (level_byte, opt_byte, payload) = match data {
        [l, o, rest @ ..] => (*l, *o, rest),
        [l] => (*l, 0, &[][..]),
        _ => (0, 0, &[][..]),
    };
    let level = Level::from_zstd(i32::from(level_byte % 23));
    let mut opts = zstdx::EncoderOptions::new(level).checksum(opt_byte & 1 == 1);
    if opt_byte & 2 != 0 {
        opts = opts.pledged_size(Some(payload.len() as u64));
    }
    // 0 keeps the row's window; 1..=15 force window log 10..=24 (the
    // format floor is 10, the engine cap 27).
    let window = (opt_byte >> 2) & 0x0f;
    if window != 0 {
        opts =
            opts.with_input_shape(zstdx::InputShape::default().with_window_log(u32::from(window) + 9));
    }
    let ldm = match (opt_byte >> 6) & 3 {
        1 => zstdx::LdmMode::Enable,
        2 => zstdx::LdmMode::Disable,
        _ => zstdx::LdmMode::Auto,
    };
    opts = opts.long_distance_matching(ldm);

    let comp = zstdx::bulk::compress_with(payload, &opts).unwrap();

    let mut back = Vec::with_capacity(payload.len() + 16);
    FrameDecoder::new()
        .decode_all_to_vec(&comp, &mut back)
        .unwrap();
    assert_eq!(&back[..], payload, "zstdx decode of own output differs");

    let mut zback = Vec::new();
    zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
    assert_eq!(&zback[..], payload, "zstd decode of zstdx output differs");
});
