//! Integration tests: exercise the exported C ABI from Rust, covering the
//! semantics real consumers rely on (buffer-advance contracts, flush
//! returns, error codes, frame metadata).

use core::ffi::{c_char, c_int};

use zstd::compress_bound;

fn compress(dst: &mut [u8], src: &[u8], level: c_int) -> usize {
    unsafe {
        zstd::ZSTD_compress(
            dst.as_mut_ptr().cast(),
            dst.len(),
            src.as_ptr().cast(),
            src.len(),
            level,
        )
    }
}

fn decompress(dst: &mut [u8], src: &[u8]) -> usize {
    unsafe {
        zstd::ZSTD_decompress(
            dst.as_mut_ptr().cast(),
            dst.len(),
            src.as_ptr().cast(),
            src.len(),
        )
    }
}

fn is_error(code: usize) -> bool {
    unsafe { zstd::ZSTD_isError(code) != 0 }
}

fn error_code(code: usize) -> u32 {
    unsafe { zstd::ZSTD_getErrorCode(code) }
}

fn error_name(code: usize) -> String {
    let name = zstd::ZSTD_getErrorName(code);
    unsafe { core::ffi::CStr::from_ptr(name as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

fn shapes() -> Vec<Vec<u8>> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    vec![
        b"".to_vec(),
        b"a".to_vec(),
        b"abcabcabcabcabcabc".to_vec(),
        vec![b'x'; 300 * 1024],
        (0..130 * 1024).map(|_| (rand() & 0xff) as u8).collect(),
        (0..900 * 1024).map(|i| (i % 61) as u8).collect(),
    ]
}

/// setParameter returns the applied value on success (the reference
/// contract); 0 means "the value cannot be represented" (negative levels).
fn set_param(cctx: *mut zstd::ZSTD_CCtx, param: c_int, value: c_int) -> usize {
    let r = unsafe { zstd::ZSTD_CCtx_setParameter(cctx, param, value) };
    assert!(!is_error(r));
    r
}

fn cctx_with(level: c_int, checksum: c_int) -> *mut zstd::ZSTD_CCtx {
    let cctx = zstd::ZSTD_createCCtx();
    assert!(!cctx.is_null());
    if level != 0 {
        assert_eq!(level as usize, set_param(cctx, 100, level));
    }
    if checksum != 0 {
        assert_eq!(1, set_param(cctx, 201, checksum));
    }
    cctx
}

fn dctx_with(window_log_max: c_int) -> *mut zstd::ZSTD_DCtx {
    let dctx = zstd::ZSTD_createDCtx();
    assert!(!dctx.is_null());
    if window_log_max != 0 {
        assert_eq!(0, unsafe {
            zstd::ZSTD_DCtx_setParameter(dctx, 100, window_log_max)
        });
    }
    dctx
}

/// Stream `input` through `ZSTD_compressStream2` with the given buffer
/// sizes, ending with e_end, and return the whole frame.
fn stream_compress(input: &[u8], level: c_int, in_size: usize, out_size: usize) -> Vec<u8> {
    let cctx = cctx_with(level, 0);
    let mut in_buf = vec![0u8; in_size.max(1)];
    let mut out_buf = vec![0u8; out_size.max(1)];
    let mut produced = Vec::new();
    let mut in_pos = 0usize;
    loop {
        let take = (input.len() - in_pos).min(in_size);
        in_buf[..take].copy_from_slice(&input[in_pos..in_pos + take]);
        let mut zin = zstd::ZSTD_inBuffer {
            src: in_buf.as_ptr().cast(),
            size: take,
            pos: 0,
        };
        let end = if in_pos + take == input.len() {
            2
        } else {
            0
        };
        loop {
            let mut zout = zstd::ZSTD_outBuffer {
                dst: out_buf.as_mut_ptr().cast(),
                size: out_buf.len(),
                pos: 0,
            };
            let r = unsafe { zstd::ZSTD_compressStream2(cctx, &mut zout, &mut zin, end) };
            assert!(!is_error(r), "compressStream2 failed");
            produced.extend_from_slice(&out_buf[..zout.pos]);
            if zin.pos == zin.size && (end == 0 || r == 0) {
                break;
            }
            assert!(
                zout.pos > 0 || zin.pos < zin.size,
                "no forward progress (in {}/{}, out {})",
                zin.pos,
                zin.size,
                zout.pos
            );
        }
        in_pos += take;
        if in_pos == input.len() {
            break;
        }
    }
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
    produced
}

#[test]
fn version_surface() {
    assert_eq!(unsafe { zstd::ZSTD_versionNumber() }, 10_600);
    assert_eq!(error_name_or_empty(), "1.6.0");
    assert_eq!(unsafe { zstd::ZSTD_versionMajor() }, 1);
    assert_eq!(unsafe { zstd::ZSTD_versionMinor() }, 6);
    assert_eq!(unsafe { zstd::ZSTD_versionRelease() }, 0);
    assert_eq!(unsafe { zstd::ZSTD_maxCLevel() }, 22);
    assert_eq!(unsafe { zstd::ZSTD_minCLevel() }, -131_072);
    assert_eq!(unsafe { zstd::ZSTD_defaultCLevel() }, 3);
    assert_eq!(unsafe { zstd::ZSTD_compressBound(0) }, 64);
    assert_eq!(unsafe { zstd::ZSTD_compressBound(1) }, 64);
    assert_eq!(unsafe { zstd::ZSTD_compressBound(131_072) }, 131_584);
}

fn error_name_or_empty() -> String {
    let v = zstd::ZSTD_versionString();
    unsafe { core::ffi::CStr::from_ptr(v as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn one_shot_roundtrip_all_levels() {
    let mut compressed = Vec::new();
    let mut plain = Vec::new();
    for input in shapes() {
        for level in [0, -3, 1, 3, 9, 19, 22, 99] {
            compressed.clear();
            compressed.resize(compress_bound(input.len()).max(64), 0);
            let n = compress(&mut compressed, &input, level);
            assert!(!is_error(n), "compress failed at level {level}");
            // The frame header declares the content size, as libzstd does.
            let fcs = unsafe { zstd::ZSTD_getFrameContentSize(compressed.as_ptr().cast(), n) };
            assert_eq!(fcs, input.len() as u64, "level {level}");

            plain.clear();
            plain.resize(input.len() + 1, 0);
            let m = decompress(&mut plain, &compressed[..n]);
            assert_eq!(m, input.len(), "level {level}");
            assert_eq!(&plain[..m], &input[..]);
        }
    }
}

#[test]
fn one_shot_walks_its_own_frame() {
    let input = b"data data data data";
    let mut dst = vec![0u8; compress_bound(input.len())];
    let n = compress(&mut dst, input, 3);
    let walked = unsafe { zstd::ZSTD_findFrameCompressedSize(dst.as_ptr().cast(), n) };
    assert_eq!(walked, n);
}

#[test]
fn one_shot_error_paths() {
    let input = b"some payload that compresses";
    let mut dst = vec![0u8; compress_bound(input.len())];
    let n = compress(&mut dst, input, 3);
    let frame = dst[..n].to_vec();

    // Truncated input.
    let code = decompress(&mut dst, &frame[..n - 2]);
    assert!(is_error(code));
    assert_eq!(error_code(code), zstd::ErrorCode::SrcSizeWrong as u32);

    // Bad magic: prefix_unknown (the reference's srcSize_wrong remap needs
    // a completed frame in front of the garbage).
    let mut garbage = frame.to_vec();
    garbage[0] ^= 0xff;
    let code = decompress(&mut dst, &garbage);
    assert!(is_error(code));
    assert_eq!(error_code(code), zstd::ErrorCode::PrefixUnknown as u32);

    // Tiny destination.
    let mut tiny = [0u8; 3];
    let code = decompress(&mut tiny, &frame);
    assert!(is_error(code));
    assert_eq!(error_code(code), zstd::ErrorCode::DstSizeTooSmall as u32);

    // The exact-size boundary decodes fine.
    let mut exact = vec![0u8; input.len()];
    let m = decompress(&mut exact, &frame);
    assert_eq!(m, input.len());
    assert_eq!(&exact, input);

    // Empty input: libzstd's multi-frame loop treats it as no frames.
    assert_eq!(decompress(&mut dst, &[]), 0);
}

#[test]
fn error_names_round_trip() {
    // NULL destination with positive capacity.
    let code = unsafe { zstd::ZSTD_compress(core::ptr::null_mut(), 4, b"x".as_ptr().cast(), 1, 3) };
    assert!(is_error(code));
    assert_eq!(error_code(code), zstd::ErrorCode::DstBufferNull as u32);
    assert_eq!(error_name(code), "Operation on NULL destination buffer");
    // A plain value names as "no error", as libzstd does for 0.
    assert_eq!(error_name(0), "No error detected");
}

#[test]
fn metadata_entries() {
    let input: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut dst = vec![0u8; compress_bound(input.len())];
    let n = compress(&mut dst, &input, 9);

    assert_eq!(unsafe { zstd::ZSTD_isFrame(dst.as_ptr().cast(), n) }, 1);
    // libzstd needs the four magic bytes before it answers.
    assert_eq!(unsafe { zstd::ZSTD_isFrame(dst.as_ptr().cast(), 3) }, 0);
    assert_eq!(unsafe { zstd::ZSTD_isFrame(dst.as_ptr().cast(), 4) }, 1);
    assert_eq!(
        unsafe { zstd::ZSTD_isFrame(b"\x1f\x8b\x08\x00garbage".as_ptr().cast(), 9) },
        0
    );
    assert_eq!(unsafe { zstd::ZSTD_isFrame(dst.as_ptr().cast(), 0) }, 0);

    let fcs = unsafe { zstd::ZSTD_getFrameContentSize(dst.as_ptr().cast(), n) };
    assert_eq!(fcs, input.len() as u64);
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameContentSize(dst.as_ptr().cast(), 3) },
        zstd::CONTENTSIZE_ERROR
    );

    let size = unsafe { zstd::ZSTD_findFrameCompressedSize(dst.as_ptr().cast(), n) };
    assert_eq!(size, n);
    let truncated = unsafe { zstd::ZSTD_findFrameCompressedSize(dst.as_ptr().cast(), n - 1) };
    assert!(is_error(truncated));

    // A frame whose opening call is a single-round e_end pledges the input
    // size (libzstd's auto-override); a chunked stream still carries none.
    let single_round = stream_compress(&input, 9, 128 * 1024, 128 * 1024);
    let fcs =
        unsafe { zstd::ZSTD_getFrameContentSize(single_round.as_ptr().cast(), single_round.len()) };
    assert_eq!(fcs, input.len() as u64);
    let chunked = stream_compress(&input, 9, 32 * 1024, 128 * 1024);
    let fcs = unsafe { zstd::ZSTD_getFrameContentSize(chunked.as_ptr().cast(), chunked.len()) };
    assert_eq!(fcs, zstd::CONTENTSIZE_UNKNOWN);
}

#[test]
fn streaming_roundtrip_tiny_buffers() {
    let big: Vec<u8> = (0..700 * 1024u32).map(|i| (i % 61) as u8).collect();
    // Big buffers first, then tiny ones on a smaller input (the one-byte
    // output steps exercise the pending-flush loop of every directive).
    let small: Vec<u8> = (0..16 * 1024u32).map(|i| (i % 97) as u8).collect();
    for (input, in_size, out_size) in [
        (&big, 128 * 1024, 128 * 1024),
        (&big, 7, 13),
        (&small, 5, 3),
    ] {
        let frame = stream_compress(input, 3, in_size, out_size);
        let mut plain = vec![0u8; input.len()];
        let m = decompress(&mut plain, &frame);
        assert_eq!(m, input.len(), "stream roundtrip {in_size}/{out_size}");
        assert_eq!(&plain, input);
    }
}

#[test]
fn flush_returns_zero_only_when_drained() {
    let input: Vec<u8> = (0..100 * 1024u32).map(|i| (i % 97) as u8).collect();
    let cctx = cctx_with(3, 0);
    let mut in_buf = input.clone();
    let mut out_buf = vec![0u8; 1024];
    let mut zin = zstd::ZSTD_inBuffer {
        src: in_buf.as_mut_ptr().cast(),
        size: in_buf.len(),
        pos: 0,
    };
    // Feed everything through e_continue.
    loop {
        let mut zout = zstd::ZSTD_outBuffer {
            dst: out_buf.as_mut_ptr().cast(),
            size: out_buf.len(),
            pos: 0,
        };
        let r = unsafe { zstd::ZSTD_compressStream(cctx, &mut zout, &mut zin) };
        assert!(!is_error(r));
        if zin.pos == zin.size {
            break;
        }
        assert!(zout.pos > 0, "e_continue made no progress");
    }
    // e_flush must reach exactly 0 through the small output buffer.
    let mut pending = usize::MAX;
    let mut flushes = 0;
    while pending != 0 {
        let mut zout = zstd::ZSTD_outBuffer {
            dst: out_buf.as_mut_ptr().cast(),
            size: out_buf.len(),
            pos: 0,
        };
        pending = unsafe { zstd::ZSTD_flushStream(cctx, &mut zout) };
        assert!(!is_error(pending));
        flushes += 1;
        assert!(flushes < 10_000, "flush never drained");
    }
    // endStream drains to exactly 0 and closes the frame.
    let mut pending = usize::MAX;
    while pending != 0 {
        let mut zout = zstd::ZSTD_outBuffer {
            dst: out_buf.as_mut_ptr().cast(),
            size: out_buf.len(),
            pos: 0,
        };
        pending = unsafe { zstd::ZSTD_endStream(cctx, &mut zout) };
        assert!(!is_error(pending));
    }
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
}

#[test]
fn stream_decode_multiframe_and_boundaries() {
    let a: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let b = b"second frame payload".to_vec();
    let mut frame = stream_compress(&a, 3, 128 * 1024, 128 * 1024);
    frame.extend(stream_compress(&b, 1, 128 * 1024, 128 * 1024));
    // A skippable frame behind the two data frames.
    frame.extend([0x50, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 9, 9, 9]);

    let dctx = dctx_with(0);
    let hint = unsafe { zstd::ZSTD_initDStream(dctx) };
    assert_eq!(hint, 128 * 1024 + 3);
    let mut out = vec![0u8; 4096];
    let mut in_chunk = vec![0u8; 1000];
    let mut decoded = Vec::new();
    let mut in_pos = 0usize;
    let mut finished = false;
    while !finished {
        let take = (frame.len() - in_pos).min(in_chunk.len());
        in_chunk[..take].copy_from_slice(&frame[in_pos..in_pos + take]);
        in_pos += take;
        let mut zin = zstd::ZSTD_inBuffer {
            src: in_chunk.as_ptr().cast(),
            size: take,
            pos: 0,
        };
        // Drive until the stream completes (r == 0); a full output buffer
        // just means another drain call, and only a part-filled buffer
        // with r > 0 pauses for more input.
        loop {
            let mut zout = zstd::ZSTD_outBuffer {
                dst: out.as_mut_ptr().cast(),
                size: out.len(),
                pos: 0,
            };
            let r = unsafe { zstd::ZSTD_decompressStream(dctx, &mut zout, &mut zin) };
            assert!(!is_error(r), "decode stream failed: {}", error_name(r));
            decoded.extend_from_slice(&out[..zout.pos]);
            if r == 0 {
                finished = true;
                break;
            }
            if zout.pos < zout.size {
                assert!(
                    zin.pos < zin.size,
                    "decoder stalled needing input after the final chunk (r={r})"
                );
                break;
            }
        }
        assert!(
            finished || in_pos < frame.len(),
            "input exhausted without a 0 return"
        );
    }
    assert_eq!(&decoded[..a.len()], &a[..]);
    assert_eq!(&decoded[a.len()..], &b);
    unsafe { zstd::ZSTD_freeDStream(dctx) };
}

#[test]
fn stream_decode_error_and_wait_paths() {
    let input = b"payload payload payload";
    let frame = stream_compress(input, 1, 128 * 1024, 128 * 1024);
    let mut out = [0u8; 128];

    // A truncated stream is not an error: the decoder waits for more input
    // and reports a positive hint (libzstd cannot tell the difference).
    let dctx = dctx_with(0);
    unsafe { zstd::ZSTD_initDStream(dctx) };
    let mut truncated = frame[..frame.len() - 1].to_vec();
    let mut zin = zstd::ZSTD_inBuffer {
        src: truncated.as_mut_ptr().cast(),
        size: truncated.len(),
        pos: 0,
    };
    let mut zout = zstd::ZSTD_outBuffer {
        dst: out.as_mut_ptr().cast(),
        size: out.len(),
        pos: 0,
    };
    let r = unsafe { zstd::ZSTD_decompressStream(dctx, &mut zout, &mut zin) };
    assert!(!is_error(r));
    assert!(r > 0);
    // The single truncated block decodes nothing; the hint asks for input.
    assert_eq!(zout.pos, 0);
    unsafe { zstd::ZSTD_freeDStream(dctx) };

    // Bad magic is prefix_unknown on the stream path.
    let dctx = dctx_with(0);
    unsafe { zstd::ZSTD_initDStream(dctx) };
    let mut garbage = frame.clone();
    garbage[0] = 0x99;
    let mut zin = zstd::ZSTD_inBuffer {
        src: garbage.as_mut_ptr().cast(),
        size: garbage.len(),
        pos: 0,
    };
    let r = unsafe { zstd::ZSTD_decompressStream(dctx, &mut zout, &mut zin) };
    assert!(is_error(r));
    assert_eq!(error_code(r), zstd::ErrorCode::PrefixUnknown as u32);
    // The failed context keeps reporting an error until reset.
    let r2 = unsafe { zstd::ZSTD_decompressStream(dctx, &mut zout, &mut zin) };
    assert!(is_error(r2));
    unsafe { zstd::ZSTD_freeDStream(dctx) };
}

#[test]
fn context_one_shot_and_parameter_bounds() {
    let input: Vec<u8> = (0..200 * 1024u32).map(|i| (i % 251) as u8).collect();
    let cctx = cctx_with(0, 1);
    let mut dst = vec![0u8; compress_bound(input.len())];
    // Level via ZSTD_compressCCtx's own argument.
    let n = unsafe {
        zstd::ZSTD_compressCCtx(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            input.as_ptr().cast(),
            input.len(),
            9,
        )
    };
    assert!(!is_error(n));
    // zstd.h: sticky parameters do not apply to the one-shot variants, so
    // the checksum flag set above must not change compressCCtx's output
    // (the reference library produces identical bytes).
    let cctx2 = cctx_with(0, 0);
    let mut dst2 = vec![0u8; compress_bound(input.len())];
    let m = unsafe {
        zstd::ZSTD_compressCCtx(
            cctx2,
            dst2.as_mut_ptr().cast(),
            dst2.len(),
            input.as_ptr().cast(),
            input.len(),
            9,
        )
    };
    assert!(!is_error(m));
    assert_eq!(n, m, "compressCCtx ignores the sticky checksum flag");

    // setParameter: levels clamp into the reference range, unknown
    // parameters stay unsupported, and flags clamp to 0/1.
    assert_eq!(22, set_param(cctx, 100, 1_000_000));
    assert_eq!(22, set_param(cctx, 100, 22));
    assert_eq!(3, set_param(cctx, 100, 0));
    assert_eq!(0, set_param(cctx, 100, -131_072));
    assert_eq!(1, set_param(cctx, 201, 2));
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_CCtx_setParameter(cctx, 101, 99) }),
        zstd::ErrorCode::ParameterOutOfBound as u32
    );
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_CCtx_setParameter(cctx, 999, 1) }),
        zstd::ErrorCode::ParameterUnsupported as u32
    );
    // Parameters lock once a frame is open.
    let mut streaming = vec![0u8; 128];
    let mut zin = zstd::ZSTD_inBuffer {
        src: input.as_ptr().cast(),
        size: 16,
        pos: 0,
    };
    let mut zout = zstd::ZSTD_outBuffer {
        dst: streaming.as_mut_ptr().cast(),
        size: streaming.len(),
        pos: 0,
    };
    unsafe { zstd::ZSTD_compressStream2(cctx, &mut zout, &mut zin, 0) };
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_CCtx_setParameter(cctx, 100, 5) }),
        zstd::ErrorCode::StageWrong as u32
    );
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
    unsafe { zstd::ZSTD_freeCCtx(cctx2) };
}

#[test]
fn dictionary_roundtrip() {
    let samples: Vec<Vec<u8>> = (0..24u32)
        .map(|i| {
            format!("record-{i:03}: the same shared preamble with a varying tail {i}\n")
                .into_bytes()
        })
        .collect();
    let raw = samples.concat();
    let dict = &raw[..raw.len().min(8 * 1024)];
    let payload = b"record-042: the same shared preamble with a varying tail 42\n".repeat(64);

    let cctx = zstd::ZSTD_createCCtx();
    let mut dst = vec![0u8; compress_bound(payload.len())];
    let n = unsafe {
        zstd::ZSTD_compress_usingDict(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payload.as_ptr().cast(),
            payload.len(),
            dict.as_ptr().cast(),
            dict.len(),
            6,
        )
    };
    assert!(!is_error(n), "{}", error_name(n));
    let dctx = zstd::ZSTD_createDCtx();
    let mut plain = vec![0u8; payload.len() + 1];
    let m = unsafe {
        zstd::ZSTD_decompress_usingDict(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            dst.as_ptr().cast(),
            n,
            dict.as_ptr().cast(),
            dict.len(),
        )
    };
    assert_eq!(m, payload.len());
    assert_eq!(&plain[..m], &payload[..]);
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
}

#[test]
fn window_log_and_nb_workers_parameters() {
    let input: Vec<u8> = (0..300 * 1024u32).map(|i| (i % 61) as u8).collect();
    let mut dst = vec![0u8; compress_bound(input.len())];
    let mut plain = vec![0u8; input.len()];

    let cctx = cctx_with(0, 0);
    assert_eq!(14, set_param(cctx, 101, 14));
    let n = unsafe {
        zstd::ZSTD_compressCCtx(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            input.as_ptr().cast(),
            input.len(),
            19,
        )
    };
    assert!(!is_error(n));
    assert_eq!(decompress(&mut plain, &dst[..n]), input.len());
    assert_eq!(&plain, &input);
    unsafe { zstd::ZSTD_freeCCtx(cctx) };

    // nbWorkers accepts sane values; MT rides the engine's job paths.
    let cctx = cctx_with(0, 0);
    assert_eq!(4, set_param(cctx, 400, 4));
    let n = unsafe {
        zstd::ZSTD_compressCCtx(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            input.as_ptr().cast(),
            input.len(),
            3,
        )
    };
    assert!(!is_error(n));
    assert_eq!(decompress(&mut plain, &dst[..n]), input.len());
    assert_eq!(&plain, &input);
    unsafe { zstd::ZSTD_freeCCtx(cctx) };

    // The decode-side windowLogMax accepts 0 (default) and rejects garbage.
    let dctx = dctx_with(0);
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_setParameter(dctx, 100, 0) });
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_DCtx_setParameter(dctx, 100, 200) }),
        zstd::ErrorCode::ParameterOutOfBound as u32
    );
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_DCtx_setParameter(dctx, 999, 1) }),
        zstd::ErrorCode::ParameterUnsupported as u32
    );
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
}

#[test]
fn free_accepts_null() {
    assert_eq!(0, unsafe { zstd::ZSTD_freeCCtx(core::ptr::null_mut()) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeDCtx(core::ptr::null_mut()) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeCStream(core::ptr::null_mut()) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeDStream(core::ptr::null_mut()) });
}

#[test]
fn stream_sizes() {
    assert_eq!(unsafe { zstd::ZSTD_CStreamInSize() }, 128 * 1024);
    assert_eq!(unsafe { zstd::ZSTD_CStreamOutSize() }, 131_591);
    assert_eq!(unsafe { zstd::ZSTD_DStreamInSize() }, 131_075);
    assert_eq!(unsafe { zstd::ZSTD_DStreamOutSize() }, 128 * 1024);
    let cstream = zstd::ZSTD_createCStream();
    assert!(!cstream.is_null());
    assert_eq!(0, unsafe { zstd::ZSTD_initCStream(cstream, 3) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeCStream(cstream) });
    let dstream = zstd::ZSTD_createDStream();
    assert!(!dstream.is_null());
    assert!(unsafe { zstd::ZSTD_initDStream(dstream) } > 0);
    assert_eq!(0, unsafe { zstd::ZSTD_freeDStream(dstream) });
}
