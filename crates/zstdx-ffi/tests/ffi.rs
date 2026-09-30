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

/// `stream_compress` over a caller-owned context: several chunked
/// e_continue rounds then e_end, so session state (a pledge, a loaded
/// dictionary) applies to the frame.
fn stream_compress_ctx(cctx: *mut zstd::ZSTD_CCtx, input: &[u8], chunk: usize) -> Vec<u8> {
    let mut produced = Vec::new();
    let mut out_buf = vec![0u8; 128 * 1024];
    let mut in_pos = 0usize;
    if input.is_empty() {
        let mut zib = zstd::ZSTD_inBuffer {
            src: core::ptr::null(),
            size: 0,
            pos: 0,
        };
        loop {
            let mut zob = zstd::ZSTD_outBuffer {
                dst: out_buf.as_mut_ptr().cast(),
                size: out_buf.len(),
                pos: 0,
            };
            let r = unsafe { zstd::ZSTD_compressStream2(cctx, &mut zob, &mut zib, 2) };
            assert!(!is_error(r));
            produced.extend_from_slice(&out_buf[..zob.pos]);
            if r == 0 {
                break;
            }
        }
        return produced;
    }
    while in_pos < input.len() {
        let take = (input.len() - in_pos).min(chunk);
        let mut zib = zstd::ZSTD_inBuffer {
            src: input[in_pos..].as_ptr().cast(),
            size: take,
            pos: 0,
        };
        in_pos += take;
        let end = if in_pos == input.len() {
            2
        } else {
            0
        };
        loop {
            let mut zob = zstd::ZSTD_outBuffer {
                dst: out_buf.as_mut_ptr().cast(),
                size: out_buf.len(),
                pos: 0,
            };
            let r = unsafe { zstd::ZSTD_compressStream2(cctx, &mut zob, &mut zib, end) };
            assert!(!is_error(r));
            produced.extend_from_slice(&out_buf[..zob.pos]);
            if zib.pos == zib.size && (end == 0 || r == 0) {
                break;
            }
        }
    }
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

fn raw_dict() -> Vec<u8> {
    // A raw-content dictionary cut from the same pattern family as the
    // payload (a formatted dictionary, when present in the tree, is used by
    // the dictID tests below).
    (0..8192u32).map(|i| (b'a' + (i % 23) as u8)).collect()
}

fn formatted_dict() -> Option<Vec<u8>> {
    // The reference-trained dictionary the zstdx dict tests carry, when
    // present (skipped otherwise, like the in-tree dict tests).
    std::fs::read("../zstdx/dict_tests/dictionary").ok()
}

#[test]
fn reset_round_trips_on_reused_cctx() {
    // A mixed payload: regular stretches the levels compress differently,
    // unlike a pure modulus pattern (levels 3 and 9 tie there).
    let a: Vec<u8> = (0..100_000u32)
        .map(|i| {
            if i % 7 < 5 {
                (b'a' + (i % 23) as u8)
            } else {
                (i >> 3) as u8 ^ 0x5a
            }
        })
        .collect();
    let b: Vec<u8> = (0..40_000u32).map(|i| (i % 97) as u8).collect();
    let cctx = zstd::ZSTD_createCCtx();
    let mut dst = vec![0u8; compress_bound(a.len())];
    let mut plain = vec![0u8; a.len().max(b.len())];

    // Frame one at level 9.
    assert_eq!(9, set_param(cctx, 100, 9));
    let n1 = unsafe {
        zstd::ZSTD_compress2(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            a.as_ptr().cast(),
            a.len(),
        )
    };
    assert!(!is_error(n1));
    assert_eq!(decompress(&mut plain, &dst[..n1]), a.len());

    // Session reset between frames; different data, same sticky level.
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_reset(cctx, 1) });
    let n2 = unsafe {
        zstd::ZSTD_compress2(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            b.as_ptr().cast(),
            b.len(),
        )
    };
    assert!(!is_error(n2));
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameContentSize(dst.as_ptr().cast(), n2) },
        b.len() as u64
    );

    // Parameters reset: the level returns to the default (pinned against a
    // fresh default-settings context) and a parameters reset mid-frame is
    // refused.
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_reset(cctx, 3) });
    let n3 = unsafe {
        zstd::ZSTD_compress2(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            a.as_ptr().cast(),
            a.len(),
        )
    };
    assert!(!is_error(n3));
    let fresh = zstd::ZSTD_createCCtx();
    let n4 = unsafe {
        zstd::ZSTD_compress2(
            fresh,
            dst.as_mut_ptr().cast(),
            dst.len(),
            a.as_ptr().cast(),
            a.len(),
        )
    };
    assert_eq!(n3, n4, "reset did not restore the default parameters");
    unsafe { zstd::ZSTD_freeCCtx(fresh) };

    // A parameters reset while a frame is open fails with stage_wrong;
    // the session-then-parameters form always succeeds.
    let mut zin = zstd::ZSTD_inBuffer {
        src: a.as_ptr().cast(),
        size: 4096,
        pos: 0,
    };
    let mut small = [0u8; 128];
    let mut zout = zstd::ZSTD_outBuffer {
        dst: small.as_mut_ptr().cast(),
        size: small.len(),
        pos: 0,
    };
    unsafe { zstd::ZSTD_compressStream2(cctx, &mut zout, &mut zin, 0) };
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_CCtx_reset(cctx, 2) }),
        zstd::ErrorCode::StageWrong as u32
    );
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_reset(cctx, 3) });

    // Unknown directive values are no-ops, as in libzstd.
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_reset(cctx, 7) });
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
}

#[test]
fn compress2_honors_sticky_params_and_loaded_dict() {
    let payload: Vec<u8> = (0..60_000u32).map(|i| (i % 251) as u8).collect();
    let dict = raw_dict();
    let cctx = zstd::ZSTD_createCCtx();
    assert_eq!(6, set_param(cctx, 100, 6));
    assert_eq!(1, set_param(cctx, 201, 1));
    let mut dst = vec![0u8; compress_bound(payload.len())];

    // Plain compress2: sticky level + checksum flag (trailer present).
    let n = unsafe {
        zstd::ZSTD_compress2(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payload.as_ptr().cast(),
            payload.len(),
        )
    };
    assert!(!is_error(n));
    let mut zfh = zstd::ZSTD_FrameHeader {
        frame_content_size: 0,
        window_size: 0,
        block_size_max: 0,
        frame_type: 0,
        header_size: 0,
        dict_id: 0,
        checksum_flag: 0,
        _reserved1: 0,
        _reserved2: 0,
    };
    let r = unsafe { zstd::ZSTD_getFrameHeader(&mut zfh, dst.as_ptr().cast(), n) };
    assert_eq!(r, 0);
    assert_eq!(zfh.frame_content_size, payload.len() as u64);
    assert_eq!(zfh.checksum_flag, 1);
    assert_eq!(zfh.frame_type, 0);
    assert!(zfh.header_size >= 5);
    // Single-segment frame: window == FCS, so the block cap is the FCS.
    assert_eq!(zfh.block_size_max as u64, zfh.frame_content_size);
    assert_eq!(zfh.dict_id, 0);

    // With a loaded dictionary: the frames compress smaller and decode
    // through the same dictionary.
    assert_eq!(0, unsafe {
        zstd::ZSTD_CCtx_loadDictionary(cctx, dict.as_ptr().cast(), dict.len())
    });
    let nd = unsafe {
        zstd::ZSTD_compress2(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payload.as_ptr().cast(),
            payload.len(),
        )
    };
    assert!(!is_error(nd), "{}", error_name(nd));
    assert!(nd < n, "dictionary must help this payload");
    let dctx = zstd::ZSTD_createDCtx();
    assert_eq!(0, unsafe {
        zstd::ZSTD_DCtx_loadDictionary(dctx, dict.as_ptr().cast(), dict.len())
    });
    let mut plain = vec![0u8; payload.len()];
    let m = unsafe {
        zstd::ZSTD_decompressDCtx(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            dst.as_ptr().cast(),
            nd,
        )
    };
    assert_eq!(m, payload.len(), "{}", error_name(m));
    assert_eq!(&plain, &payload);

    // A parameters reset drops the dictionary and the checksum flag.
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_reset(cctx, 3) });
    let n2 = unsafe {
        zstd::ZSTD_compress2(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payload.as_ptr().cast(),
            payload.len(),
        )
    };
    assert!(!is_error(n2));
    assert!(n2 > nd, "dictionary survived the parameters reset");
    // No checksum trailer anymore: identical bytes to the flag-less frame.
    let cctx2 = zstd::ZSTD_createCCtx();
    assert_eq!(3, set_param(cctx2, 100, 3));
    let n4 = unsafe {
        zstd::ZSTD_compress2(
            cctx2,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payload.as_ptr().cast(),
            payload.len(),
        )
    };
    assert_eq!(n2, n4);

    // Tiny destination.
    let mut tiny = [0u8; 4];
    let r2 = unsafe {
        zstd::ZSTD_compress2(
            cctx2,
            tiny.as_mut_ptr().cast(),
            tiny.len(),
            payload.as_ptr().cast(),
            payload.len(),
        )
    };
    assert_eq!(error_code(r2), zstd::ErrorCode::DstSizeTooSmall as u32);
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
    unsafe { zstd::ZSTD_freeCCtx(cctx2) };
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
}

#[test]
fn pledged_size_on_streaming_frames() {
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 61) as u8).collect();
    let chunk = 64 * 1024;
    let cctx = zstd::ZSTD_createCCtx();
    assert_eq!(5, set_param(cctx, 100, 5));

    // Chunked streaming with a pledge: the header declares it.
    assert_eq!(0, unsafe {
        zstd::ZSTD_CCtx_setPledgedSrcSize(cctx, payload.len() as u64)
    });
    let frame = stream_compress_ctx(cctx, &payload, chunk);
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameContentSize(frame.as_ptr().cast(), frame.len()) },
        payload.len() as u64
    );
    let mut plain = vec![0u8; payload.len()];
    assert_eq!(decompress(&mut plain, &frame), payload.len());
    assert_eq!(&plain, &payload);

    // The pledge is consumed by the frame; the next one declares nothing.
    let frame2 = stream_compress_ctx(cctx, &payload, chunk);
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameContentSize(frame2.as_ptr().cast(), frame2.len()) },
        zstd::CONTENTSIZE_UNKNOWN
    );

    // UNKNOWN means "no pledge" (the default), 0 pledges an empty frame.
    assert_eq!(0, unsafe {
        zstd::ZSTD_CCtx_setPledgedSrcSize(cctx, zstd::CONTENTSIZE_UNKNOWN)
    });
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_setPledgedSrcSize(cctx, 0) });
    let empty = stream_compress_ctx(cctx, &[], chunk);
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameContentSize(empty.as_ptr().cast(), empty.len()) },
        0
    );

    // Setting the pledge mid-frame is stage_wrong.
    let cctx2 = zstd::ZSTD_createCCtx();
    let mut zin = zstd::ZSTD_inBuffer {
        src: payload.as_ptr().cast(),
        size: 8192,
        pos: 0,
    };
    let mut small = [0u8; 256];
    let mut zout = zstd::ZSTD_outBuffer {
        dst: small.as_mut_ptr().cast(),
        size: small.len(),
        pos: 0,
    };
    unsafe { zstd::ZSTD_compressStream2(cctx2, &mut zout, &mut zin, 0) };
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_CCtx_setPledgedSrcSize(cctx2, 5) }),
        zstd::ErrorCode::StageWrong as u32
    );
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
    unsafe { zstd::ZSTD_freeCCtx(cctx2) };
}

#[test]
fn cdict_reuse_matches_using_dict() {
    let dict = raw_dict();
    let payloads: Vec<Vec<u8>> = (1..=3u32)
        .map(|k| (0..50_000u32).map(|i| (i % (61 + k)) as u8).collect())
        .collect();

    let cdict = unsafe { zstd::ZSTD_createCDict(dict.as_ptr().cast(), dict.len(), 7) };
    assert!(!cdict.is_null());
    // A raw-content dictionary carries no dictID.
    assert_eq!(0, unsafe { zstd::ZSTD_getDictID_fromCDict(cdict) });

    let cctx = zstd::ZSTD_createCCtx();
    let mut via_cdict = Vec::new();
    for payload in &payloads {
        let mut dst = vec![0u8; compress_bound(payload.len())];
        let n = unsafe {
            zstd::ZSTD_compress_usingCDict(
                cctx,
                dst.as_mut_ptr().cast(),
                dst.len(),
                payload.as_ptr().cast(),
                payload.len(),
                cdict,
            )
        };
        assert!(!is_error(n), "{}", error_name(n));
        // Every frame declares its content size (usingCDict's hardcoded
        // frame parameters).
        assert_eq!(
            unsafe { zstd::ZSTD_getFrameContentSize(dst.as_ptr().cast(), n) },
            payload.len() as u64
        );
        via_cdict.push(dst[..n].to_vec());
    }

    // The same compressions through usingDict must be byte-identical.
    for (payload, frame) in payloads.iter().zip(&via_cdict) {
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
                7,
            )
        };
        assert_eq!(&dst[..n], frame, "usingCDict and usingDict diverged");
    }

    // A NULL cdict is dictionary_wrong, per the reference.
    let mut dst = vec![0u8; compress_bound(16)];
    let r = unsafe {
        zstd::ZSTD_compress_usingCDict(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payloads[0].as_ptr().cast(),
            payloads[0].len(),
            core::ptr::null(),
        )
    };
    assert_eq!(error_code(r), zstd::ErrorCode::DictionaryWrong as u32);

    // An empty CDict transports only its level and compresses fine.
    let empty = unsafe { zstd::ZSTD_createCDict(core::ptr::null(), 0, 3) };
    assert!(!empty.is_null());
    let mut big = vec![0u8; compress_bound(payloads[0].len())];
    let n = unsafe {
        zstd::ZSTD_compress_usingCDict(
            cctx,
            big.as_mut_ptr().cast(),
            big.len(),
            payloads[0].as_ptr().cast(),
            payloads[0].len(),
            empty,
        )
    };
    assert!(!is_error(n), "{}", error_name(n));

    // free(NULL) is a no-op.
    assert_eq!(0, unsafe { zstd::ZSTD_freeCDict(core::ptr::null_mut()) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeCDict(cdict) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeCDict(empty) });
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
}

#[test]
fn ddict_decodes_dict_frames() {
    let dict = raw_dict();
    let payload: Vec<u8> = {
        // Payload prefixed by the dictionary content so frames reference it.
        let mut p = dict.clone();
        p.extend((0..40_000u32).map(|i| (i % 61) as u8));
        p
    };
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
    assert!(!is_error(n));

    let ddict = unsafe { zstd::ZSTD_createDDict(dict.as_ptr().cast(), dict.len()) };
    assert!(!ddict.is_null());
    assert_eq!(0, unsafe { zstd::ZSTD_getDictID_fromDDict(ddict) });

    let dctx = zstd::ZSTD_createDCtx();
    let mut plain = vec![0u8; payload.len()];
    // Through the DDict handle...
    let m = unsafe {
        zstd::ZSTD_decompress_usingDDict(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            dst.as_ptr().cast(),
            n,
            ddict,
        )
    };
    assert_eq!(m, payload.len(), "{}", error_name(m));
    assert_eq!(&plain, &payload);
    // ...and through bytes: identical results.
    let m2 = unsafe {
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
    assert_eq!(m2, payload.len());

    // A NULL DDict decodes plain and fails on the dict-referencing frame.
    let r = unsafe {
        zstd::ZSTD_decompress_usingDDict(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            dst.as_ptr().cast(),
            n,
            core::ptr::null(),
        )
    };
    assert!(is_error(r));

    assert_eq!(0, unsafe { zstd::ZSTD_freeDDict(core::ptr::null_mut()) });
    assert_eq!(0, unsafe { zstd::ZSTD_freeDDict(ddict) });
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
}

#[test]
fn load_dictionary_stage_and_stickiness() {
    // Pseudo-random bytes (incompressible on their own) ending with a copy
    // of the dictionary's head: only a decoder holding the dictionary can
    // regenerate the tail cheaply, and the encoder references the dict.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 32) as u8
    };
    let dict: Vec<u8> = (0..8192u32).map(|_| rnd()).collect();
    // A short head of fresh bytes, then the dictionary's tail: the tail is
    // only compressible through dictionary matches a short offset back.
    let mut payload: Vec<u8> = (0..1000u32).map(|_| rnd()).collect();
    payload.extend_from_slice(&dict[4096..]);

    // CCtx: mid-frame load is refused; after a session reset it applies.
    let cctx = zstd::ZSTD_createCCtx();
    let mut zin = zstd::ZSTD_inBuffer {
        src: payload.as_ptr().cast(),
        size: 8192,
        pos: 0,
    };
    let mut small = [0u8; 256];
    let mut zout = zstd::ZSTD_outBuffer {
        dst: small.as_mut_ptr().cast(),
        size: small.len(),
        pos: 0,
    };
    unsafe { zstd::ZSTD_compressStream2(cctx, &mut zout, &mut zin, 0) };
    assert_eq!(
        error_code(unsafe {
            zstd::ZSTD_CCtx_loadDictionary(cctx, dict.as_ptr().cast(), dict.len())
        }),
        zstd::ErrorCode::StageWrong as u32
    );
    assert_eq!(0, unsafe { zstd::ZSTD_CCtx_reset(cctx, 1) });
    assert_eq!(0, unsafe {
        zstd::ZSTD_CCtx_loadDictionary(cctx, dict.as_ptr().cast(), dict.len())
    });
    // The loaded dictionary also rides the streaming entries.
    let frame = {
        let mut produced = Vec::new();
        let mut in_pos = 0usize;
        while in_pos < payload.len() {
            let take = (payload.len() - in_pos).min(16 * 1024);
            let mut zib = zstd::ZSTD_inBuffer {
                src: payload[in_pos..].as_ptr().cast(),
                size: take,
                pos: 0,
            };
            in_pos += take;
            loop {
                let mut buf = [0u8; 4096];
                let mut zob = zstd::ZSTD_outBuffer {
                    dst: buf.as_mut_ptr().cast(),
                    size: buf.len(),
                    pos: 0,
                };
                let end = if in_pos == payload.len() {
                    2
                } else {
                    0
                };
                let r = unsafe { zstd::ZSTD_compressStream2(cctx, &mut zob, &mut zib, end) };
                assert!(!is_error(r));
                produced.extend_from_slice(&buf[..zob.pos]);
                if zib.pos == zib.size && (end == 0 || r == 0) {
                    break;
                }
            }
        }
        produced
    };
    let dctx = zstd::ZSTD_createDCtx();
    assert_eq!(0, unsafe {
        zstd::ZSTD_DCtx_loadDictionary(dctx, dict.as_ptr().cast(), dict.len())
    });
    let mut plain = vec![0u8; payload.len()];
    let m = unsafe {
        zstd::ZSTD_decompressDCtx(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            frame.as_ptr().cast(),
            frame.len(),
        )
    };
    assert_eq!(m, payload.len(), "{}", error_name(m));

    // initDStream drops the dictionary (legacy contract): a dict frame
    // no longer decodes through the context.
    unsafe { zstd::ZSTD_initDStream(dctx) };
    let r = unsafe {
        zstd::ZSTD_decompressDCtx(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            frame.as_ptr().cast(),
            frame.len(),
        )
    };
    assert!(is_error(r));

    // A NULL dictionary returns to no-dictionary mode.
    assert_eq!(0, unsafe {
        zstd::ZSTD_CCtx_loadDictionary(cctx, core::ptr::null(), 0)
    });
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
}

#[test]
fn dctx_reset_directives() {
    let payload: Vec<u8> = (0..30_000u32).map(|i| (i % 61) as u8).collect();
    let mut dst = vec![0u8; compress_bound(payload.len())];
    let n = compress(&mut dst, &payload, 3);
    let dctx = zstd::ZSTD_createDCtx();
    let mut plain = vec![0u8; payload.len()];

    // Parameters mid-decode are refused, session reset is not, and after
    // the session reset the parameters reset succeeds.
    let mut zin = zstd::ZSTD_inBuffer {
        src: dst.as_ptr().cast(),
        size: n,
        pos: 0,
    };
    let mut zout = zstd::ZSTD_outBuffer {
        dst: plain.as_mut_ptr().cast(),
        size: 100,
        pos: 0,
    };
    unsafe { zstd::ZSTD_initDStream(dctx) };
    unsafe { zstd::ZSTD_decompressStream(dctx, &mut zout, &mut zin) };
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_DCtx_reset(dctx, 2) }),
        zstd::ErrorCode::StageWrong as u32
    );
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_reset(dctx, 1) });
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_reset(dctx, 2) });
    // Unknown directive values are no-ops.
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_reset(dctx, 42) });

    // windowLogMax survives a session reset and drops on a parameters one.
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_setParameter(dctx, 100, 20) });
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_reset(dctx, 1) });
    assert_eq!(0, unsafe { zstd::ZSTD_DCtx_reset(dctx, 2) });
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
}

#[test]
fn metadata_size_entries() {
    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut dst = vec![0u8; compress_bound(payload.len())];
    let n = compress(&mut dst, &payload, 9);

    // The obsolete blend: known sizes survive, everything else collapses.
    assert_eq!(
        unsafe { zstd::ZSTD_getDecompressedSize(dst.as_ptr().cast(), n) },
        payload.len() as u64
    );
    let mut empty = vec![0u8; 64];
    let en = compress(&mut empty, &[], 3);
    assert_eq!(
        unsafe { zstd::ZSTD_getDecompressedSize(empty.as_ptr().cast(), en) },
        0
    );

    // findDecompressedSize sums series, propagates unknown, rejects junk.
    assert_eq!(
        unsafe { zstd::ZSTD_findDecompressedSize(dst.as_ptr().cast(), n) },
        payload.len() as u64
    );
    let mut series = dst[..n].to_vec();
    let skip: Vec<u8> = [0x50u8, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 9, 9, 9].to_vec();
    series.extend_from_slice(&skip);
    assert_eq!(
        unsafe { zstd::ZSTD_findDecompressedSize(series.as_ptr().cast(), series.len()) },
        payload.len() as u64
    );
    let unknown = stream_compress(&payload, 9, 32 * 1024, 128 * 1024);
    assert_eq!(
        unsafe { zstd::ZSTD_findDecompressedSize(unknown.as_ptr().cast(), unknown.len()) },
        zstd::CONTENTSIZE_UNKNOWN
    );
    let mut trailing = dst[..n].to_vec();
    trailing.extend_from_slice(&[1, 2]);
    assert_eq!(
        unsafe { zstd::ZSTD_findDecompressedSize(trailing.as_ptr().cast(), trailing.len()) },
        zstd::CONTENTSIZE_ERROR
    );
    assert_eq!(
        unsafe { zstd::ZSTD_findDecompressedSize(dst.as_ptr().cast(), 0) },
        0
    );

    // frameHeaderSize: the descriptor arithmetic, srcSize_wrong below 5.
    let r = unsafe { zstd::ZSTD_frameHeaderSize(dst.as_ptr().cast(), n) };
    assert!(!is_error(r));
    assert!(r >= 5 && r <= 18);
    let mut zfh = zstd::ZSTD_FrameHeader {
        frame_content_size: 0,
        window_size: 0,
        block_size_max: 0,
        frame_type: 0,
        header_size: 0,
        dict_id: 0,
        checksum_flag: 0,
        _reserved1: 0,
        _reserved2: 0,
    };
    let g = unsafe { zstd::ZSTD_getFrameHeader(&mut zfh, dst.as_ptr().cast(), n) };
    assert_eq!(g, 0);
    assert_eq!(zfh.header_size as usize, r);
    // Short inputs ask for the prefix; garbage is prefix_unknown.
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameHeader(&mut zfh, dst.as_ptr().cast(), 0) },
        5
    );
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameHeader(&mut zfh, dst.as_ptr().cast(), 3) },
        5
    );
    let garbage = [9u8, 9, 9];
    assert!(is_error(unsafe {
        zstd::ZSTD_getFrameHeader(&mut zfh, garbage.as_ptr().cast(), 3)
    }));
    assert_eq!(
        error_code(unsafe { zstd::ZSTD_getFrameHeader(&mut zfh, garbage.as_ptr().cast(), 3) }),
        zstd::ErrorCode::PrefixUnknown as u32
    );

    // Skippable frames through every metadata entry.
    assert_eq!(
        unsafe { zstd::ZSTD_getFrameContentSize(skip.as_ptr().cast(), skip.len()) },
        0
    );
    assert_eq!(
        unsafe { zstd::ZSTD_getDecompressedSize(skip.as_ptr().cast(), skip.len()) },
        0
    );
    let sh = unsafe { zstd::ZSTD_frameHeaderSize(skip.as_ptr().cast(), skip.len()) };
    let g = unsafe { zstd::ZSTD_getFrameHeader(&mut zfh, skip.as_ptr().cast(), skip.len()) };
    assert_eq!(g, 0);
    assert_eq!(zfh.frame_type, 1);
    assert_eq!(zfh.frame_content_size, 3);
    assert_eq!(zfh.header_size, 8);
    assert_eq!(zfh.dict_id, 0);
    // The reference applies the descriptor arithmetic to src[4] whatever
    // the magic is: size byte 3 parses as a windowed header with a 4-byte
    // dictID field -> 10.
    assert_eq!(sh, 10);

    // getDictID_fromDict / fromFrame.
    assert_eq!(
        unsafe { zstd::ZSTD_getDictID_fromDict(skip.as_ptr().cast(), skip.len()) },
        0
    );
    assert_eq!(
        unsafe { zstd::ZSTD_getDictID_fromDict(core::ptr::null(), 0) },
        0
    );
    assert_eq!(
        unsafe { zstd::ZSTD_getDictID_fromFrame(dst.as_ptr().cast(), n) },
        0
    );
    assert_eq!(
        unsafe { zstd::ZSTD_getDictID_fromFrame(dst.as_ptr().cast(), 3) },
        0
    );
}

#[test]
fn formatted_dict_ids_flow_into_headers() {
    let Some(dict) = formatted_dict() else {
        return;
    };
    if dict.len() < 8 {
        return;
    }
    let id = unsafe { zstd::ZSTD_getDictID_fromDict(dict.as_ptr().cast(), dict.len()) };
    assert!(id != 0, "the trained dictionary must carry an id");

    let payload: Vec<u8> = {
        let mut files: Vec<_> = std::fs::read_dir("../zstdx/dict_tests/files")
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        files.sort();
        match files.first() {
            Some(path) => std::fs::read(path).unwrap_or_else(|_| dict[..dict.len() / 2].to_vec()),
            None => dict[..dict.len() / 2].to_vec(),
        }
    };

    // CDict compression declares the dictID; DDict decodes by it.
    let cdict = unsafe { zstd::ZSTD_createCDict(dict.as_ptr().cast(), dict.len(), 5) };
    assert!(!cdict.is_null());
    assert_eq!(id, unsafe { zstd::ZSTD_getDictID_fromCDict(cdict) });
    let ddict = unsafe { zstd::ZSTD_createDDict(dict.as_ptr().cast(), dict.len()) };
    assert_eq!(id, unsafe { zstd::ZSTD_getDictID_fromDDict(ddict) });

    let cctx = zstd::ZSTD_createCCtx();
    let mut dst = vec![0u8; compress_bound(payload.len())];
    let n = unsafe {
        zstd::ZSTD_compress_usingCDict(
            cctx,
            dst.as_mut_ptr().cast(),
            dst.len(),
            payload.as_ptr().cast(),
            payload.len(),
            cdict,
        )
    };
    assert!(!is_error(n), "{}", error_name(n));
    assert_eq!(
        unsafe { zstd::ZSTD_getDictID_fromFrame(dst.as_ptr().cast(), n) },
        id
    );

    // A decoder without the dictionary refuses the frame.
    let dctx = zstd::ZSTD_createDCtx();
    let mut plain = vec![0u8; payload.len()];
    let r = unsafe {
        zstd::ZSTD_decompressDCtx(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            dst.as_ptr().cast(),
            n,
        )
    };
    assert_eq!(error_code(r), zstd::ErrorCode::DictionaryWrong as u32);
    // Through the DDict it round-trips.
    let m = unsafe {
        zstd::ZSTD_decompress_usingDDict(
            dctx,
            plain.as_mut_ptr().cast(),
            plain.len(),
            dst.as_ptr().cast(),
            n,
            ddict,
        )
    };
    assert_eq!(m, payload.len(), "{}", error_name(m));
    assert_eq!(&plain[..m], &payload[..]);

    unsafe { zstd::ZSTD_freeCDict(cdict) };
    unsafe { zstd::ZSTD_freeDDict(ddict) };
    unsafe { zstd::ZSTD_freeDCtx(dctx) };
    unsafe { zstd::ZSTD_freeCCtx(cctx) };
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
