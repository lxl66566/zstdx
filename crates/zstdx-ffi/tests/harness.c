/*
 * C harness for the zstdx FFI layer: linked against the reference libzstd
 * (static) or the zstdx-built libzstd.so (drop-in), it runs the same
 * self-contained checks and, via the produce/consume modes, verifies
 * interop in both directions (see run-c.sh).
 *
 * Build (self):  gcc -O2 -DZSTD_STATIC_LINKING_ONLY -I <ref-zstd>/lib \
 *                    harness.c -o harness-self -L <zstdx>/target/release \
 *                    -l:libzstd.so
 * Build (ref):   gcc -O2 -DZSTD_STATIC_LINKING_ONLY -I <ref-zstd>/lib \
 *                    harness.c <ref-zstd>/lib/libzstd.a -lpthread -lm
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#include "zstd.h"

static int failures = 0;

#define CHECK(cond, ...)                                                       \
    do {                                                                       \
        if (!(cond)) {                                                         \
            failures++;                                                        \
            printf("FAIL %s:%d: ", __FILE__, __LINE__);                        \
            printf(__VA_ARGS__);                                               \
            printf("\n");                                                      \
        }                                                                      \
    } while (0)

/* Deterministic corpus: compressible mix plus incompressible runs. */
static unsigned long long rng_state = 0x9e3779b97f4a7c15ULL;
static unsigned rnd(void) {
    rng_state ^= rng_state << 13;
    rng_state ^= rng_state >> 7;
    rng_state ^= rng_state << 17;
    return (unsigned)(rng_state >> 32);
}

static unsigned char *gen_input(size_t n) {
    unsigned char *buf = malloc(n ? n : 1);
    for (size_t i = 0; i < n; i++) {
        buf[i] = (i % 7 < 5) ? (unsigned char)('a' + (i % 23)) : (unsigned char)(rnd() & 0xff);
    }
    return buf;
}

static void test_version(void) {
    CHECK(ZSTD_versionNumber() == 10600, "version %u", ZSTD_versionNumber());
    CHECK(strcmp(ZSTD_versionString(), "1.6.0") == 0, "version string %s", ZSTD_versionString());
    CHECK(ZSTD_maxCLevel() == 22, "maxCLevel");
    CHECK(ZSTD_defaultCLevel() == 3, "defaultCLevel");
    CHECK(ZSTD_compressBound(0) == 64, "bound(0)=%zu", ZSTD_compressBound(0));
    CHECK(ZSTD_compressBound(131072) == 131584, "bound(128K)=%zu", ZSTD_compressBound(131072));
    CHECK(ZSTD_CStreamOutSize() == 131591, "CStreamOutSize=%zu", ZSTD_CStreamOutSize());
    CHECK(ZSTD_DStreamInSize() == 131075, "DStreamInSize=%zu", ZSTD_DStreamInSize());
}

static void test_one_shot(void) {
    const size_t sizes[] = {0, 1, 64, 4096, 200000, 1500000};
    const int levels[] = {-5, 0, 1, 3, 9, 19, 22};
    for (size_t si = 0; si < sizeof(sizes) / sizeof(sizes[0]); si++) {
        size_t n = sizes[si];
        unsigned char *src = gen_input(n);
        size_t cap = ZSTD_compressBound(n);
        unsigned char *cmp = malloc(cap + 1);
        unsigned char *out = malloc(n + 1);
        for (size_t li = 0; li < sizeof(levels) / sizeof(levels[0]); li++) {
            int level = levels[li];
            size_t c = ZSTD_compress(cmp, cap, src, n, level);
            CHECK(!ZSTD_isError(c), "compress n=%zu level=%d: %s", n, level,
                  ZSTD_getErrorName(c));
            if (ZSTD_isError(c)) continue;
            CHECK(ZSTD_getFrameContentSize(cmp, c) == n,
                  "content size n=%zu level=%d: %llu", n, level,
                  ZSTD_getFrameContentSize(cmp, c));
            CHECK(ZSTD_findFrameCompressedSize(cmp, c) == c, "frame walk n=%zu", n);
            size_t d = ZSTD_decompress(out, n + 1, cmp, c);
            CHECK(!ZSTD_isError(d), "decompress n=%zu level=%d: %s", n, level,
                  ZSTD_getErrorName(d));
            CHECK(d == n && (n == 0 || memcmp(out, src, n) == 0), "roundtrip n=%zu", n);
        }
        free(src);
        free(cmp);
        free(out);
    }
}

static void test_errors(void) {
    unsigned char *src = gen_input(10000);
    size_t cap = ZSTD_compressBound(10000);
    unsigned char *cmp = malloc(cap);
    unsigned char *out = malloc(10001);
    size_t c = ZSTD_compress(cmp, cap, src, 10000, 3);
    CHECK(!ZSTD_isError(c), "setup");

    size_t r = ZSTD_decompress(out, 10001, cmp, c - 1);
    CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_srcSize_wrong,
          "truncated: %s", ZSTD_getErrorName(r));
    cmp[0] ^= 0xff;
    r = ZSTD_decompress(out, 10001, cmp, c);
    CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_prefix_unknown,
          "bad magic: %s", ZSTD_getErrorName(r));
    cmp[0] ^= 0xff;
    r = ZSTD_decompress(out, 100, cmp, c);
    CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_dstSize_tooSmall,
          "tiny dst: %s", ZSTD_getErrorName(r));
    r = ZSTD_compress(NULL, 10, src, 100, 1);
    CHECK(ZSTD_isError(r), "null dst");
    /* Error fidelity (R67): gap values keep their number through
     * getErrorCode, -(maxCode) itself is not an error, and both facts
     * surface through the name table. */
    CHECK(ZSTD_isError(0) == 0, "isError 0");
    CHECK(ZSTD_getErrorCode((size_t)-22) == ZSTD_error_checksum_wrong, "code -22");
    CHECK(ZSTD_getErrorCode((size_t)-11) == 11, "gap code keeps number");
    CHECK(ZSTD_getErrorCode((size_t)-119) == 119, "gap edge keeps number");
    CHECK(ZSTD_isError((size_t)-120) == 0, "-(maxCode) not an error");
    CHECK(strcmp(ZSTD_getErrorName((size_t)-120), "No error detected") == 0,
          "-(maxCode) name: %s", ZSTD_getErrorName((size_t)-120));
    CHECK(strcmp(ZSTD_getErrorName((size_t)-11), "Unspecified error code") == 0,
          "gap name: %s", ZSTD_getErrorName((size_t)-11));
    CHECK(ZSTD_isFrame(NULL, 0) == 0, "isFrame null");
    CHECK(ZSTD_isFrame(cmp, 3) == 0, "isFrame short");
    CHECK(ZSTD_isFrame(cmp, 4) == 1, "isFrame magic");
    CHECK(ZSTD_freeCCtx(NULL) == 0 && ZSTD_freeDCtx(NULL) == 0, "free null");

    free(src);
    free(cmp);
    free(out);
}

static void test_contexts(void) {
    ZSTD_CCtx *cctx = ZSTD_createCCtx();
    ZSTD_DCtx *dctx = ZSTD_createDCtx();
    size_t n = 700000;
    unsigned char *src = gen_input(n);
    size_t cap = ZSTD_compressBound(n);
    unsigned char *cmp = malloc(cap);
    unsigned char *out = malloc(n);

    /* setParameter returns the applied value on success (the reference
     * contract since zstd 1.6). */
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, 9) == 9, "level param");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, 1) == 1, "checksum param");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_dictIDFlag, 1) == 1, "dictID param");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_dictIDFlag, 0) == 0, "dictID off");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_enableLongDistanceMatching, 1) == 1, "ldm param");
    CHECK(ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_nbWorkers, 2)) ||
          ZSTD_CCtx_setParameter(cctx, ZSTD_c_nbWorkers, 2) == 2, "workers param");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_windowLog, 18) == 18, "window param");
    CHECK(ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_windowLog, 99)), "window oob");
    /* hashLog: this layer models a parameter subset; libzstd accepts it. */
    CHECK(ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_hashLog, 17)) ||
          ZSTD_CCtx_setParameter(cctx, ZSTD_c_hashLog, 17) == 17, "hashLog");

    /* d windowLogMax bounds (R67): {0} + 10..=31, the reference dParam
     * bound; unknown d params are parameter_unsupported. */
    {
        ZSTD_DCtx *d2 = ZSTD_createDCtx();
        CHECK(ZSTD_DCtx_setParameter(d2, ZSTD_d_windowLogMax, 0) == 0, "dwlm 0");
        CHECK(ZSTD_DCtx_setParameter(d2, ZSTD_d_windowLogMax, 27) == 0, "dwlm 27");
        CHECK(ZSTD_DCtx_setParameter(d2, ZSTD_d_windowLogMax, 31) == 0, "dwlm 31");
        size_t rd = ZSTD_DCtx_setParameter(d2, ZSTD_d_windowLogMax, 32);
        CHECK(ZSTD_isError(rd) && ZSTD_getErrorCode(rd) == ZSTD_error_parameter_outOfBound,
              "dwlm 32: %s", ZSTD_getErrorName(rd));
        rd = ZSTD_DCtx_setParameter(d2, ZSTD_d_windowLogMax, 9);
        CHECK(ZSTD_isError(rd) && ZSTD_getErrorCode(rd) == ZSTD_error_parameter_outOfBound,
              "dwlm 9: %s", ZSTD_getErrorName(rd));
        rd = ZSTD_DCtx_setParameter(d2, 999999, 1);
        CHECK(ZSTD_isError(rd) && ZSTD_getErrorCode(rd) == ZSTD_error_parameter_unsupported,
              "dwlm unknown: %s", ZSTD_getErrorName(rd));
        ZSTD_freeDCtx(d2);
    }

    size_t c = ZSTD_compressCCtx(cctx, cmp, cap, src, n, 9);
    CHECK(!ZSTD_isError(c), "compressCCtx: %s", ZSTD_getErrorName(c));
    size_t d = ZSTD_decompressDCtx(dctx, out, n, cmp, c);
    CHECK(!ZSTD_isError(d) && d == n && memcmp(out, src, n) == 0, "ctx roundtrip");
    CHECK(ZSTD_getFrameContentSize(cmp, c) == n, "ctx content size");

    /* windowLog cap visible in the header-declared window: re-compress at
     * windowLog 10 and confirm the frames still roundtrip both ways. */
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_windowLog, 10) == 10, "window 10");
    size_t c2 = ZSTD_compressCCtx(cctx, cmp, cap, src, n, 6);
    CHECK(!ZSTD_isError(c2), "compressCCtx w10: %s", ZSTD_getErrorName(c2));
    size_t d2 = ZSTD_decompressDCtx(dctx, out, n, cmp, c2);
    CHECK(!ZSTD_isError(d2) && d2 == n && memcmp(out, src, n) == 0, "w10 roundtrip");
    CHECK(c2 > c, "forced tiny window must compress worse: %zu vs %zu", c2, c);

    /* dictionary roundtrip */
    unsigned char *dict = gen_input(4096);
    size_t cd = ZSTD_compress_usingDict(cctx, cmp, cap, src, n, dict, 4096, 5);
    CHECK(!ZSTD_isError(cd), "compress_usingDict: %s", ZSTD_getErrorName(cd));
    size_t dd = ZSTD_decompress_usingDict(dctx, out, n, cmp, cd, dict, 4096);
    CHECK(!ZSTD_isError(dd) && dd == n && memcmp(out, src, n) == 0, "dict roundtrip");

    ZSTD_freeCCtx(cctx);
    ZSTD_freeDCtx(dctx);
    free(src);
    free(cmp);
    free(out);
    free(dict);
}

/* Streaming compression with hostile buffer sizes; returns the frame. */
static unsigned char *stream_frame(const unsigned char *src, size_t n, int level,
                                   size_t in_step, size_t out_step, size_t *out_len) {
    ZSTD_CCtx *cctx = ZSTD_createCStream();
    CHECK(ZSTD_initCStream(cctx, level) == 0, "initCStream");
    unsigned char *frame = malloc(ZSTD_compressBound(n) + 1024);
    unsigned char *out_chunk = malloc(out_step);
    size_t produced = 0, fed = 0;
    ZSTD_EndDirective end = ZSTD_e_continue;
    for (;;) {
        size_t take = n - fed < in_step ? n - fed : in_step;
        ZSTD_inBuffer in = {src + fed, take, 0};
        fed += take;
        if (fed == n) end = ZSTD_e_end;
        for (;;) {
            ZSTD_outBuffer out = {out_chunk, out_step, 0};
            size_t r = ZSTD_compressStream2(cctx, &out, &in, end);
            CHECK(!ZSTD_isError(r), "compressStream2: %s", ZSTD_getErrorName(r));
            memcpy(frame + produced, out_chunk, out.pos);
            produced += out.pos;
            if (in.pos == in.size && (end == ZSTD_e_continue || r == 0)) break;
            CHECK(out.pos > 0 || in.pos < in.size, "no progress");
        }
        if (end == ZSTD_e_end) break;
    }
    ZSTD_freeCStream(cctx);
    free(out_chunk);
    *out_len = produced;
    return frame;
}

static void test_streaming(void) {
    size_t n = 900000;
    unsigned char *src = gen_input(n);
    unsigned char *out = malloc(n);
    unsigned char *ref = malloc(ZSTD_compressBound(n));

    size_t c_ref = ZSTD_compress(ref, ZSTD_compressBound(n), src, n, 5);
    CHECK(!ZSTD_isError(c_ref), "ref compress");

    /* Streaming frames carry no pledged content size (no
     * ZSTD_CCtx_setPledgedSrcSize here), so they legitimately differ from
     * the one-shot frame; what must hold is determinism across buffer
     * chunkings, since the encoder never sees them. */
    size_t clen0 = 0;
    unsigned char *frame0 = stream_frame(src, n, 5, 128 * 1024, 128 * 1024, &clen0);
    struct { size_t in_step, out_step; } steps[] = {
        {1024, 1024}, {63, 17}, {1, 128}, {128, 1},
    };
    for (size_t i = 0; i < sizeof(steps) / sizeof(steps[0]); i++) {
        size_t clen = 0;
        unsigned char *frame = stream_frame(src, n, 5, steps[i].in_step, steps[i].out_step, &clen);
        CHECK(clen == clen0, "streaming determinism step %zu: %zu vs %zu", i, clen, clen0);
        size_t d = ZSTD_decompress(out, n, frame, clen);
        CHECK(!ZSTD_isError(d) && d == n && memcmp(out, src, n) == 0, "stream roundtrip step %zu",
              i);
        free(frame);
    }
    {
        size_t d = ZSTD_decompress(out, n, frame0, clen0);
        CHECK(!ZSTD_isError(d) && d == n && memcmp(out, src, n) == 0, "stream roundtrip base");
    }
    free(frame0);

    /* decompressStream through a tiny output buffer */
    {
        size_t clen = 0;
        unsigned char *frame = stream_frame(src, n, 5, 64 * 1024, 64 * 1024, &clen);
        ZSTD_DStream *zds = ZSTD_createDStream();
        CHECK(ZSTD_initDStream(zds) > 0, "initDStream");
        unsigned char *chunk = malloc(1000);
        unsigned char *dec = malloc(n + 16);
        size_t decoded = 0, fed = 0;
        for (;;) {
            size_t take = clen - fed < 1000 ? clen - fed : 1000;
            ZSTD_inBuffer in = {frame + fed, take, 0};
            fed += take;
            for (;;) {
                ZSTD_outBuffer ob = {chunk, 1000, 0};
                size_t r = ZSTD_decompressStream(zds, &ob, &in);
                CHECK(!ZSTD_isError(r), "decompressStream: %s", ZSTD_getErrorName(r));
                memcpy(dec + decoded, chunk, ob.pos);
                decoded += ob.pos;
                if (r == 0) goto done;
                /* Part-filled output with r > 0: waiting for more input. */
                if (ob.pos < ob.size) break;
            }
        }
    done:
        CHECK(decoded == n && memcmp(dec, src, n) == 0, "stream decode roundtrip");
        ZSTD_freeDStream(zds);
        free(chunk);
        free(dec);
        free(frame);
    }

    /* e_flush returns 0 only when fully flushed (1-byte out buffer) */
    {
        ZSTD_CCtx *cctx = ZSTD_createCStream();
        ZSTD_initCStream(cctx, 3);
        ZSTD_inBuffer in = {src, 4096, 0};
        unsigned char ob1[1];
        for (;;) {
            ZSTD_outBuffer ob = {ob1, 1, 0};
            size_t r = ZSTD_compressStream2(cctx, &ob, &in, ZSTD_e_continue);
            CHECK(!ZSTD_isError(r), "continue: %s", ZSTD_getErrorName(r));
            if (in.pos == in.size) break;
        }
        size_t guard = 0, pending = SIZE_MAX;
        while (pending != 0) {
            ZSTD_outBuffer ob = {ob1, 1, 0};
            pending = ZSTD_flushStream(cctx, &ob);
            CHECK(!ZSTD_isError(pending), "flush: %s", ZSTD_getErrorName(pending));
            if (++guard > 1000000) { CHECK(0, "flush never drained"); break; }
        }
        guard = 0, pending = SIZE_MAX;
        while (pending != 0) {
            ZSTD_outBuffer ob = {ob1, 1, 0};
            pending = ZSTD_endStream(cctx, &ob);
            CHECK(!ZSTD_isError(pending), "end: %s", ZSTD_getErrorName(pending));
            if (++guard > 1000000) { CHECK(0, "end never drained"); break; }
        }
        ZSTD_freeCStream(cctx);
    }

    /* Wild cursors are rejected like the reference (R67): compressStream2
     * checks output first, input second; decompressStream checks input
     * first. */
    {
        ZSTD_CCtx *cctx = ZSTD_createCStream();
        ZSTD_initCStream(cctx, 3);
        unsigned char ob[64];
        ZSTD_inBuffer in = {src, 64, 65};
        ZSTD_outBuffer wout = {ob, sizeof(ob), sizeof(ob) + 1};
        size_t rc1 = ZSTD_compressStream2(cctx, &wout, &in, ZSTD_e_continue);
        CHECK(ZSTD_isError(rc1) && ZSTD_getErrorCode(rc1) == ZSTD_error_dstSize_tooSmall,
              "c wild out cursor: %s", ZSTD_getErrorName(rc1));
        ZSTD_outBuffer ok = {ob, sizeof(ob), 0};
        size_t rc2 = ZSTD_compressStream2(cctx, &ok, &in, ZSTD_e_continue);
        CHECK(ZSTD_isError(rc2) && ZSTD_getErrorCode(rc2) == ZSTD_error_srcSize_wrong,
              "c wild in cursor: %s", ZSTD_getErrorName(rc2));
        ZSTD_freeCStream(cctx);

        ZSTD_DStream *zds2 = ZSTD_createDStream();
        ZSTD_initDStream(zds2);
        ZSTD_outBuffer dwout = {ob, sizeof(ob), sizeof(ob) + 1};
        ZSTD_inBuffer dwin = {src, 64, 65};
        size_t rd1 = ZSTD_decompressStream(zds2, &dwout, &dwin);
        CHECK(ZSTD_isError(rd1) && ZSTD_getErrorCode(rd1) == ZSTD_error_srcSize_wrong,
              "d wild in cursor: %s", ZSTD_getErrorName(rd1));
        ZSTD_inBuffer dok = {src, 64, 0};
        size_t rd2 = ZSTD_decompressStream(zds2, &dwout, &dok);
        CHECK(ZSTD_isError(rd2) && ZSTD_getErrorCode(rd2) == ZSTD_error_dstSize_tooSmall,
              "d wild out cursor: %s", ZSTD_getErrorName(rd2));
        ZSTD_freeDStream(zds2);
    }

    free(src);
    free(out);
    free(ref);
}

/* ---- file interop modes ---- */

static unsigned char *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) { printf("FAIL cannot open %s\n", path); exit(2); }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *buf = malloc((size_t)n + 1);
    if (fread(buf, 1, (size_t)n, f) != (size_t)n) { printf("FAIL short read\n"); exit(2); }
    fclose(f);
    *len = (size_t)n;
    return buf;
}

static int produce(const char *raw_path, const char *zst_path, int level, int stream) {
    size_t n;
    unsigned char *src = read_file(raw_path, &n);
    FILE *f = fopen(zst_path, "wb");
    if (!f) { printf("FAIL cannot write %s\n", zst_path); return 2; }
    if (!stream) {
        size_t cap = ZSTD_compressBound(n);
        unsigned char *cmp = malloc(cap);
        size_t c = ZSTD_compress(cmp, cap, src, n, level);
        if (ZSTD_isError(c)) { printf("FAIL produce: %s\n", ZSTD_getErrorName(c)); return 2; }
        fwrite(cmp, 1, c, f);
    } else {
        ZSTD_CCtx *cctx = ZSTD_createCStream();
        ZSTD_initCStream(cctx, level);
        unsigned char *chunk = malloc(ZSTD_CStreamOutSize());
        size_t fed = 0;
        while (fed < n) {
            size_t take = n - fed < 65536 ? n - fed : 65536;
            ZSTD_inBuffer in = {src + fed, take, 0};
            fed += take;
            ZSTD_EndDirective end = fed == n ? ZSTD_e_end : ZSTD_e_continue;
            for (;;) {
                ZSTD_outBuffer out = {chunk, (size_t)ZSTD_CStreamOutSize(), 0};
                size_t r = ZSTD_compressStream2(cctx, &out, &in, end);
                if (ZSTD_isError(r)) { printf("FAIL produce stream: %s\n", ZSTD_getErrorName(r)); return 2; }
                fwrite(chunk, 1, out.pos, f);
                if (in.pos == in.size && (end == ZSTD_e_continue || r == 0)) break;
            }
        }
        ZSTD_freeCStream(cctx);
    }
    fclose(f);
    free(src);
    return 0;
}

static int consume(const char *zst_path, const char *raw_path) {
    size_t n, rn;
    unsigned char *cmp = read_file(zst_path, &n);
    unsigned char *raw = read_file(raw_path, &rn);
    unsigned long long fcs = ZSTD_getFrameContentSize(cmp, n);
    size_t cap = (size_t)(fcs > 0 && fcs != (unsigned long long)-1 ? fcs : rn + 64);
    unsigned char *out = malloc(cap ? cap : 1);
    size_t d = ZSTD_decompress(out, cap, cmp, n);
    if (ZSTD_isError(d)) { printf("FAIL consume: %s\n", ZSTD_getErrorName(d)); return 2; }
    CHECK(d == rn && memcmp(out, raw, rn) == 0, "consume mismatch (%zu vs %zu)", d, rn);

    /* Same bytes through the streaming decoder. */
    ZSTD_DStream *zds = ZSTD_createDStream();
    ZSTD_initDStream(zds);
    unsigned char *chunk = malloc((size_t)ZSTD_DStreamOutSize());
    unsigned char *dec = malloc(d ? d : 1);
    size_t decoded = 0, fed = 0;
    for (;;) {
        size_t take = n - fed < 4096 ? n - fed : 4096;
        ZSTD_inBuffer in = {cmp + fed, take, 0};
        fed += take;
        for (;;) {
            ZSTD_outBuffer ob = {chunk, (size_t)ZSTD_DStreamOutSize(), 0};
            size_t r = ZSTD_decompressStream(zds, &ob, &in);
            if (ZSTD_isError(r)) { printf("FAIL consume stream: %s\n", ZSTD_getErrorName(r)); return 2; }
            memcpy(dec + decoded, chunk, ob.pos);
            decoded += ob.pos;
            if (r == 0) goto done;
            if (ob.pos < ob.size) break;
        }
    }
done:
    CHECK(decoded == rn && memcmp(dec, raw, rn) == 0, "consume stream mismatch");
    ZSTD_freeDStream(zds);
    free(chunk);
    free(dec);
    free(cmp);
    free(raw);
    free(out);
    printf(failures ? "consume FAILED\n" : "consume OK (%zu bytes)\n", rn);
    return failures ? 1 : 0;
}

/* Reset family + compress2: sticky parameters, session/parameter resets,
 * pledged sizes on chunked and single-round streams. */

/* CDict/DDict objects, loadDictionary, and the getDictID family. */

/* DCtx reset + the metadata size entries. */

/* Deterministic incompressible bytes (dictionary tests need content the
 * pattern generator cannot accidentally reproduce). */
static unsigned char *gen_random(size_t n) {
    unsigned char *buf = malloc(n ? n : 1);
    for (size_t i = 0; i < n; i++) {
        rnd(); /* churn */
        buf[i] = (unsigned char)(rng_state >> 32);
    }
    return buf;
}

/* Reset family + compress2: sticky parameters, session/parameter resets,
 * pledged sizes on chunked and single-round streams. */
static void test_reset_compress2(void) {
    ZSTD_CCtx *cctx = ZSTD_createCCtx();
    size_t n = 200000;
    unsigned char *src = gen_input(n);
    size_t cap = ZSTD_compressBound(n);
    unsigned char *cmp = malloc(cap);
    unsigned char *out = malloc(n + 1);

    /* compress2 with sticky parameters: checksum flag lands in the frame. */
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, 9) == 9, "level param");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, 1) == 1, "checksum param");
    size_t c = ZSTD_compress2(cctx, cmp, cap, src, n);
    CHECK(!ZSTD_isError(c), "compress2: %s", ZSTD_getErrorName(c));
    CHECK(ZSTD_getFrameContentSize(cmp, c) == n, "compress2 fcs");
    {
        ZSTD_FrameHeader zfh;
        memset(&zfh, 0, sizeof(zfh));
        CHECK(ZSTD_getFrameHeader(&zfh, cmp, c) == 0, "getFrameHeader");
        CHECK(zfh.checksumFlag == 1, "checksum flag in frame");
        CHECK(zfh.frameContentSize == n, "zfh fcs");
        CHECK(zfh.frameType == ZSTD_frame, "zfh type");
    }

    /* Session reset keeps parameters: the next frame still checksums. */
    CHECK(ZSTD_CCtx_reset(cctx, ZSTD_reset_session_only) == 0, "session reset");
    size_t c2 = ZSTD_compress2(cctx, cmp, cap, src, n / 2);
    CHECK(!ZSTD_isError(c2), "compress2 after reset: %s", ZSTD_getErrorName(c2));
    CHECK(ZSTD_getFrameHeader(NULL, cmp, 0) != 0, "null zfh rejected");
    {
        ZSTD_FrameHeader zfh;
        memset(&zfh, 0, sizeof(zfh));
        CHECK(ZSTD_getFrameHeader(&zfh, cmp, c2) == 0, "getHeader 2");
        CHECK(zfh.checksumFlag == 1, "checksum survives session reset");
    }

    /* Parameters reset mid-frame fails; the combined form always works. */
    {
        ZSTD_CCtx *c3 = ZSTD_createCCtx();
        ZSTD_inBuffer in = {src, 8192, 0};
        unsigned char ob[256];
        ZSTD_outBuffer outb = {ob, sizeof(ob), 0};
        ZSTD_compressStream2(c3, &outb, &in, ZSTD_e_continue);
        size_t r = ZSTD_CCtx_reset(c3, ZSTD_reset_parameters);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_stage_wrong,
              "params reset mid-frame: %s", ZSTD_getErrorName(r));
        /* Level is the reference-authorized mid-frame update (applies to
         * the next frame here); frame-shape parameters and unknown ids
         * stay init-stage only. */
        r = ZSTD_CCtx_setParameter(c3, ZSTD_c_compressionLevel, 5);
        CHECK(!ZSTD_isError(r) && r == 5, "level mid-frame: %s", ZSTD_getErrorName(r));
        r = ZSTD_CCtx_setParameter(c3, ZSTD_c_windowLog, 20);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_stage_wrong,
              "windowLog mid-frame: %s", ZSTD_getErrorName(r));
        r = ZSTD_CCtx_setParameter(c3, 999999, 1);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_stage_wrong,
              "unknown mid-frame: %s", ZSTD_getErrorName(r));
        CHECK(ZSTD_CCtx_reset(c3, ZSTD_reset_session_and_parameters) == 0, "combined reset");
        /* Unknown directive values are no-ops, as in libzstd. */
        CHECK(ZSTD_CCtx_reset(c3, (ZSTD_ResetDirective)7) == 0, "bogus directive");
        ZSTD_freeCCtx(c3);
    }

    /* Pledged size on a chunked stream: declared, then consumed. */
    CHECK(ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters) == 0, "full reset");
    CHECK(ZSTD_CCtx_setPledgedSrcSize(cctx, 100000) == 0, "pledge");
    {
        ZSTD_CCtx *cs = ZSTD_createCStream();
        CHECK(ZSTD_CCtx_setPledgedSrcSize(cs, 100000) == 0, "pledge cs");
        unsigned char *frame = malloc(ZSTD_compressBound(100000) + 64);
        size_t produced = 0, fed = 0;
        for (;;) {
            size_t take = 100000 - fed < 16384 ? 100000 - fed : 16384;
            ZSTD_inBuffer in = {src + fed, take, 0};
            fed += take;
            ZSTD_EndDirective end = fed == 100000 ? ZSTD_e_end : ZSTD_e_continue;
            for (;;) {
                ZSTD_outBuffer out = {frame + produced, 4096, 0};
                size_t r = ZSTD_compressStream2(cs, &out, &in, end);
                CHECK(!ZSTD_isError(r), "pledged stream: %s", ZSTD_getErrorName(r));
                produced += out.pos;
                if (in.pos == in.size && (end == ZSTD_e_continue || r == 0)) break;
            }
            if (end == ZSTD_e_end) break;
        }
        CHECK(ZSTD_getFrameContentSize(frame, produced) == 100000, "pledged fcs declared");
        size_t d = ZSTD_decompress(out, n, frame, produced);
        CHECK(!ZSTD_isError(d) && d == 100000 && memcmp(out, src, 100000) == 0, "pledged roundtrip");
        /* A new frame on the same context: the pledge is gone. */
        produced = 0; fed = 0;
        for (;;) {
            size_t take = 100000 - fed < 16384 ? 100000 - fed : 16384;
            ZSTD_inBuffer in = {src + fed, take, 0};
            fed += take;
            ZSTD_EndDirective end = fed == 100000 ? ZSTD_e_end : ZSTD_e_continue;
            for (;;) {
                ZSTD_outBuffer out = {frame + produced, 4096, 0};
                size_t r = ZSTD_compressStream2(cs, &out, &in, end);
                CHECK(!ZSTD_isError(r), "second stream: %s", ZSTD_getErrorName(r));
                produced += out.pos;
                if (in.pos == in.size && (end == ZSTD_e_continue || r == 0)) break;
            }
            if (end == ZSTD_e_end) break;
        }
        CHECK(ZSTD_getFrameContentSize(frame, produced) == ZSTD_CONTENTSIZE_UNKNOWN,
              "pledge consumed");
        /* UNKNOWN explicitly means no pledge. */
        CHECK(ZSTD_CCtx_reset(cs, ZSTD_reset_session_only) == 0, "cs reset");
        CHECK(ZSTD_CCtx_setPledgedSrcSize(cs, ZSTD_CONTENTSIZE_UNKNOWN) == 0, "pledge unknown");
        /* Setting a pledge mid-frame is stage_wrong. */
        {
            ZSTD_inBuffer in = {src, 8192, 0};
            unsigned char ob[256];
            ZSTD_outBuffer outb = {ob, sizeof(ob), 0};
            ZSTD_compressStream2(cs, &outb, &in, ZSTD_e_continue);
            size_t r = ZSTD_CCtx_setPledgedSrcSize(cs, 5);
            CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_stage_wrong,
                  "pledge mid-frame: %s", ZSTD_getErrorName(r));
        }
        free(frame);
        ZSTD_freeCStream(cs);
    }

    /* A single-round e_end stream declares its size (the auto-pledge). */
    {
        ZSTD_CCtx *cs = ZSTD_createCStream();
        ZSTD_inBuffer in = {src, 4096, 0};
        unsigned char *frame = malloc(ZSTD_compressBound(4096));
        ZSTD_outBuffer out = {frame, ZSTD_compressBound(4096), 0};
        size_t r;
        do {
            out.pos = 0;
            r = ZSTD_compressStream2(cs, &out, &in, ZSTD_e_end);
            CHECK(!ZSTD_isError(r), "single round: %s", ZSTD_getErrorName(r));
        } while (r != 0);
        CHECK(ZSTD_getFrameContentSize(frame, out.pos) == 4096, "single-round fcs");
        free(frame);
        ZSTD_freeCStream(cs);
    }

    ZSTD_freeCCtx(cctx);
    free(src);
    free(cmp);
    free(out);
}

/* CDict/DDict objects, loadDictionary, and the getDictID family. */
static void test_dicts(void) {
    /* A dictionary of incompressible content; the payload ends with its
     * tail so frames genuinely reference the dictionary. */
    unsigned char *dict = gen_random(8192);
    size_t payload_len = 1000 + 4096;
    unsigned char *payload = malloc(payload_len);
    memcpy(payload, gen_random(1000), 1000);
    memcpy(payload + 1000, dict + 4096, 4096);

    size_t cap = ZSTD_compressBound(payload_len);
    unsigned char *cmp = malloc(cap);
    unsigned char *cmp2 = malloc(cap);
    unsigned char *out = malloc(payload_len + 1);

    /* getDictID_fromDict: raw content and handcrafted formatted heads. */
    CHECK(ZSTD_getDictID_fromDict(dict, 8192) == 0, "raw dict id 0");
    CHECK(ZSTD_getDictID_fromDict(dict, 4) == 0, "short dict id 0");
    CHECK(ZSTD_getDictID_fromDict(NULL, 0) == 0, "null dict id 0");
    {
        unsigned char crafted[64];
        memset(crafted, 0, sizeof(crafted));
        crafted[0] = 0x37; crafted[1] = 0xA4; crafted[2] = 0x30; crafted[3] = 0xEC;
        crafted[7] = 0x5A; /* id 0x5A000000, little endian */
        CHECK(ZSTD_getDictID_fromDict(crafted, sizeof(crafted)) == 0x5A000000u, "crafted id");
    }

    ZSTD_CCtx *cctx = ZSTD_createCCtx();
    ZSTD_DCtx *dctx = ZSTD_createDCtx();

    /* CDict: level from the handle, byte-identical to usingDict. */
    ZSTD_CDict *cdict = ZSTD_createCDict(dict, 8192, 7);
    CHECK(cdict != NULL, "createCDict");
    CHECK(ZSTD_getDictID_fromCDict(cdict) == 0, "cdict raw id");
    size_t c = ZSTD_compress_usingCDict(cctx, cmp, cap, payload, payload_len, cdict);
    CHECK(!ZSTD_isError(c), "usingCDict: %s", ZSTD_getErrorName(c));
    CHECK(ZSTD_getFrameContentSize(cmp, c) == payload_len, "usingCDict fcs");
    size_t c2 = ZSTD_compress_usingDict(cctx, cmp2, cap, payload, payload_len, dict, 8192, 7);
    CHECK(!ZSTD_isError(c2), "usingDict: %s", ZSTD_getErrorName(c2));
    CHECK(c == c2 && memcmp(cmp, cmp2, c) == 0, "usingCDict == usingDict");
    /* Reuse: a second job through the same handle. */
    size_t c3 = ZSTD_compress_usingCDict(cctx, cmp2, cap, payload, payload_len - 16, cdict);
    CHECK(!ZSTD_isError(c3), "cdict reuse: %s", ZSTD_getErrorName(c3));
    /* NULL cdict is dictionary_wrong. */
    size_t r = ZSTD_compress_usingCDict(cctx, cmp2, cap, payload, 100, NULL);
    CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_dictionary_wrong,
          "usingCDict NULL: %s", ZSTD_getErrorName(r));
    /* Empty CDict: level-only handle. */
    ZSTD_CDict *empty = ZSTD_createCDict(NULL, 0, 3);
    CHECK(empty != NULL, "createCDict empty");
    r = ZSTD_compress_usingCDict(cctx, cmp2, cap, payload, 1000, empty);
    CHECK(!ZSTD_isError(r), "usingCDict empty: %s", ZSTD_getErrorName(r));
    CHECK(ZSTD_freeCDict(cdict) == 0 && ZSTD_freeCDict(empty) == 0, "freeCDict");
    CHECK(ZSTD_freeCDict(NULL) == 0, "freeCDict null");

    /* The frame needs its dictionary: plain decode fails. */
    r = ZSTD_decompressDCtx(dctx, out, payload_len + 1, cmp, c);
    CHECK(ZSTD_isError(r), "plain decode of dict frame must fail");
    /* DDict decodes it. */
    ZSTD_DDict *ddict = ZSTD_createDDict(dict, 8192);
    CHECK(ddict != NULL, "createDDict");
    CHECK(ZSTD_getDictID_fromDDict(ddict) == 0, "ddict raw id");
    size_t d = ZSTD_decompress_usingDDict(dctx, out, payload_len + 1, cmp, c, ddict);
    CHECK(!ZSTD_isError(d) && d == payload_len && memcmp(out, payload, payload_len) == 0,
          "decompress_usingDDict: %s", ZSTD_getErrorName(d));
    /* bytes form: identical outcome. */
    size_t d2 = ZSTD_decompress_usingDict(dctx, out, payload_len + 1, cmp, c, dict, 8192);
    CHECK(!ZSTD_isError(d2) && d2 == payload_len, "decompress_usingDict");
    CHECK(ZSTD_freeDDict(ddict) == 0 && ZSTD_freeDDict(NULL) == 0, "freeDDict");

    /* loadDictionary: compression and decode through the context slot. */
    CHECK(ZSTD_CCtx_loadDictionary(cctx, dict, 8192) == 0, "cctx loadDictionary");
    /* Mid-frame load is stage_wrong. */
    {
        ZSTD_CCtx *cm = ZSTD_createCCtx();
        ZSTD_inBuffer in = {payload, 8192, 0};
        unsigned char ob[256];
        ZSTD_outBuffer outb = {ob, sizeof(ob), 0};
        ZSTD_compressStream2(cm, &outb, &in, ZSTD_e_continue);
        r = ZSTD_CCtx_loadDictionary(cm, dict, 8192);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_stage_wrong,
              "loadDictionary mid-frame: %s", ZSTD_getErrorName(r));
        ZSTD_freeCCtx(cm);
    }
    size_t cl = ZSTD_compress2(cctx, cmp, cap, payload, payload_len);
    CHECK(!ZSTD_isError(cl), "compress2 with loaded dict: %s", ZSTD_getErrorName(cl));
    CHECK(cl < ZSTD_compress(cctx ? cmp2 : cmp2, cap, payload, payload_len, 7),
          "loaded dictionary must help this payload");
    CHECK(ZSTD_DCtx_loadDictionary(dctx, dict, 8192) == 0, "dctx loadDictionary");
    size_t dl = ZSTD_decompressDCtx(dctx, out, payload_len + 1, cmp, cl);
    CHECK(!ZSTD_isError(dl) && dl == payload_len, "decompressDCtx with loaded dict");
    /* A parameters reset drops the dictionary. */
    CHECK(ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters) == 0, "reset drops dict");
    size_t cn = ZSTD_compress2(cctx, cmp, cap, payload, payload_len);
    CHECK(!ZSTD_isError(cn), "compress2 after reset");
    CHECK(cn > cl, "dictionary survived the parameters reset");
    /* initDStream clears the decode-side dictionary (legacy contract). */
    CHECK(ZSTD_initDStream(dctx) > 0, "initDStream");
    r = ZSTD_decompressDCtx(dctx, out, payload_len + 1, cmp, cl);
    CHECK(ZSTD_isError(r), "dict frame must fail after initDStream");
    /* NULL dictionary returns to no-dictionary mode. */
    CHECK(ZSTD_DCtx_loadDictionary(dctx, NULL, 0) == 0, "null loadDictionary");

    /* getDictID_fromFrame: no dictID in these frames (raw dict). */
    CHECK(ZSTD_getDictID_fromFrame(cmp, cn) == 0, "fromFrame raw");
    CHECK(ZSTD_getDictID_fromFrame(cmp, 3) == 0, "fromFrame short");

    ZSTD_freeCCtx(cctx);
    ZSTD_freeDCtx(dctx);
    free(dict);
    free(payload);
    free(cmp);
    free(cmp2);
    free(out);
}

/* DCtx reset + the metadata size entries. */
static void test_dctx_reset_and_metadata(void) {
    size_t n = 30000;
    unsigned char *src = gen_input(n);
    size_t cap = ZSTD_compressBound(n);
    unsigned char *cmp = malloc(cap);
    unsigned char *out = malloc(n + 1);
    size_t c = ZSTD_compress(cmp, cap, src, n, 3);

    ZSTD_DCtx *dctx = ZSTD_createDCtx();
    /* Parameters reset mid-decode is stage_wrong; session reset then works. */
    {
        ZSTD_inBuffer in = {cmp, c, 0};
        ZSTD_outBuffer outb = {out, 100, 0};
        CHECK(ZSTD_initDStream(dctx) > 0, "initDStream");
        ZSTD_decompressStream(dctx, &outb, &in);
        size_t r = ZSTD_DCtx_reset(dctx, ZSTD_reset_parameters);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_stage_wrong,
              "dctx params reset mid-frame: %s", ZSTD_getErrorName(r));
        CHECK(ZSTD_DCtx_reset(dctx, ZSTD_reset_session_only) == 0, "dctx session reset");
        CHECK(ZSTD_DCtx_reset(dctx, ZSTD_reset_parameters) == 0, "dctx params reset");
        CHECK(ZSTD_DCtx_reset(dctx, (ZSTD_ResetDirective)42) == 0, "dctx bogus directive");
    }

    /* The obsolete getDecompressedSize blend. */
    CHECK(ZSTD_getDecompressedSize(cmp, c) == n, "getDecompressedSize known");
    {
        unsigned char empty[64];
        size_t en = ZSTD_compress(empty, 64, src, 0, 3);
        CHECK(ZSTD_getDecompressedSize(empty, en) == 0, "getDecompressedSize empty");
    }

    /* findDecompressedSize: single, series with skippable, junk, unknown. */
    CHECK(ZSTD_findDecompressedSize(cmp, c) == n, "find one");
    {
        unsigned char skip[12] = {0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4};
        unsigned char *series = malloc(c + 12);
        memcpy(series, cmp, c);
        memcpy(series + c, skip, 12);
        CHECK(ZSTD_findDecompressedSize(series, c + 12) == n, "find series+skip");
        CHECK(ZSTD_getFrameContentSize(skip, 12) == 0, "skippable fcs 0");
        CHECK(ZSTD_getDecompressedSize(skip, 12) == 0, "skippable getDecompressedSize");
        series[c] = 1; series[c + 1] = 2; /* trailing junk below prefix len */
        CHECK(ZSTD_findDecompressedSize(series, c + 2) == ZSTD_CONTENTSIZE_ERROR, "find junk");
        free(series);
        /* getFrameHeader on the skippable frame. */
        {
            ZSTD_FrameHeader zfh;
            memset(&zfh, 0, sizeof(zfh));
            CHECK(ZSTD_getFrameHeader(&zfh, skip, 12) == 0, "skippable header");
            CHECK(zfh.frameType == ZSTD_skippableFrame, "skippable type");
            CHECK(zfh.frameContentSize == 4, "skippable zfh fcs");
            CHECK(zfh.headerSize == 8, "skippable zfh headerSize");
            CHECK(zfh.dictID == 0, "skippable variant");
        }
    }
    CHECK(ZSTD_findDecompressedSize(cmp, 0) == 0, "find empty input");

    /* frameHeaderSize: descriptor arithmetic, parity on skippables. */
    {
        size_t h = ZSTD_frameHeaderSize(cmp, c);
        CHECK(!ZSTD_isError(h) && h >= 5 && h <= 18, "frameHeaderSize zstd");
        ZSTD_FrameHeader zfh;
        memset(&zfh, 0, sizeof(zfh));
        CHECK(ZSTD_getFrameHeader(&zfh, cmp, c) == 0, "getFrameHeader zstd");
        CHECK(zfh.headerSize == h, "headerSize agrees");
        CHECK(zfh.windowSize >= n / 2, "window at least content"); /* single-segment or wide */
        CHECK(zfh.blockSizeMax == (zfh.windowSize < 131072 ? zfh.windowSize : 131072),
              "blockSizeMax = min(window, 128K)");
        unsigned char skip4[12] = {0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4};
        CHECK(ZSTD_frameHeaderSize(skip4, 12) == 6, "frameHeaderSize skippable (size byte 4)");
        CHECK(ZSTD_isError(ZSTD_frameHeaderSize(cmp, 4)), "frameHeaderSize short");
        CHECK(ZSTD_getErrorCode(ZSTD_frameHeaderSize(cmp, 4)) == ZSTD_error_srcSize_wrong,
              "frameHeaderSize short code");
    }

    /* getFrameHeader: short inputs want the prefix, garbage is an error. */
    {
        ZSTD_FrameHeader zfh;
        memset(&zfh, 0, sizeof(zfh));
        CHECK(ZSTD_getFrameHeader(&zfh, cmp, 0) == 5, "empty wants 5");
        CHECK(ZSTD_getFrameHeader(&zfh, cmp, 3) == 5, "short wants 5");
        unsigned char half[3] = {0x28, 0xB5, 0x2F};
        CHECK(ZSTD_getFrameHeader(&zfh, half, 3) == 5, "magic prefix wants 5");
        unsigned char bad[3] = {1, 2, 3};
        size_t r = ZSTD_getFrameHeader(&zfh, bad, 3);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_prefix_unknown,
              "bad prefix: %s", ZSTD_getErrorName(r));
        /* Any skippable-variant first byte qualifies (masked compare). */
        unsigned char v1[1] = {0x5F};
        CHECK(ZSTD_getFrameHeader(&zfh, v1, 1) == 5, "variant byte wants 5");
    }

    /* getFrameHeader short-input contract (R67): a complete 4-byte magic of
     * either family still wants 5, never the family own size; the struct
     * write follows the reference memset placement - untouched below the
     * prefix size, zeroed whole (tail padding included) from it on. */
    {
        ZSTD_FrameHeader zfh;
        const unsigned char zm[4] = {0x28, 0xB5, 0x2F, 0xFD};
        const unsigned char sm[4] = {0x50, 0x2A, 0x4D, 0x18};
        const unsigned char *p;
        size_t i;
        memset(&zfh, 0xAA, sizeof(zfh));
        CHECK(ZSTD_getFrameHeader(&zfh, zm, 4) == 5, "zstd magic wants 5");
        CHECK(ZSTD_getFrameHeader(&zfh, sm, 4) == 5, "skippable magic wants 5");
        p = (const unsigned char *)&zfh;
        for (i = 0; i < sizeof(zfh); i++)
            if (p[i] != 0xAA) break;
        CHECK(i == sizeof(zfh), "short input leaves the struct untouched");
        memset(&zfh, 0xAA, sizeof(zfh));
        {
            size_t want = ZSTD_getFrameHeader(&zfh, cmp, 5);
            CHECK(!ZSTD_isError(want) && want >= 6 && want <= 18,
                  "incomplete header wants its size: %zu", want);
        }
        p = (const unsigned char *)&zfh;
        for (i = 0; i < sizeof(zfh); i++)
            if (p[i] != 0) break;
        CHECK(i == sizeof(zfh), "past-prefix input zeroes the struct");
        /* null src: size 0 wants 5 (no deref); positive size is GENERIC,
         * the reference entry check. */
        CHECK(ZSTD_getFrameHeader(&zfh, NULL, 0) == 5, "null src size 0 wants 5");
        {
            size_t rn = ZSTD_getFrameHeader(&zfh, NULL, 4);
            CHECK(ZSTD_isError(rn) && ZSTD_getErrorCode(rn) == ZSTD_error_GENERIC,
                  "null src: %s", ZSTD_getErrorName(rn));
        }
    }

    ZSTD_freeDCtx(dctx);
    free(src);
    free(cmp);
    free(out);
}

/* R67 round 2: one-shot bad-magic gating matrix, the null-buffer
 * family, the deprecated DStream init entries, and the streaming
 * no-forward-progress guard. Every expectation below is reference
 * behavior (the suite runs against libzstd.a first). */
static void test_r67_round2(void) {
    unsigned char payload[4096];
    for (size_t i = 0; i < 4096; i++) payload[i] = (unsigned char)((i * 7) % 251);
    size_t cap = ZSTD_compressBound(4096);
    unsigned char *cmp = malloc(cap);
    unsigned char *out = malloc(8192);
    size_t c = ZSTD_compress(cmp, cap, payload, 4096, 5);
    CHECK(!ZSTD_isError(c), "setup: %s", ZSTD_getErrorName(c));
    unsigned char skip[12] = {0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4};

    /* Bad-magic tails: the descriptor byte drives the frame-header size,
     * so a garbage tail is believed (prefix_unknown) only past
     * fhsize + blockHeader (9 bare, 17 with an 8-byte FCS); below that it
     * is leftover input. A decoded frame in front promotes any tail to
     * srcSize_wrong; a skippable frame never promotes. */
    {
        const unsigned char fhd[2] = {0x00, 0xC0};
        for (int d = 0; d < 2; d++) {
            size_t gate = (d ? 14 : 6) + 3;
            for (size_t len = 0; len <= gate + 2; len++) {
                unsigned char g[32];
                g[0] = 0x11; g[1] = 0x22; g[2] = 0x33; g[3] = 0x44;
                for (size_t i = 4; i < len && i < 32; i++)
                    g[i] = i == 4 ? fhd[d] : (unsigned char)i;
                unsigned want_short = ZSTD_error_srcSize_wrong;
                unsigned want_long = ZSTD_error_prefix_unknown;
                if (len == 0) {
                    CHECK(ZSTD_decompress(out, 8192, g, len) == 0, "empty input");
                    continue;
                }
                size_t r = ZSTD_decompress(out, 8192, g, len);
                CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) ==
                      (len < gate ? want_short : want_long),
                      "gar-only d=%d len=%zu: %s", d, len, ZSTD_getErrorName(r));
                unsigned char *ser = malloc(c + len);
                memcpy(ser, cmp, c);
                memcpy(ser + c, g, len);
                r = ZSTD_decompress(out, 8192, ser, c + len);
                CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == want_short,
                      "frm+gar d=%d len=%zu: %s", d, len, ZSTD_getErrorName(r));
                memcpy(ser, skip, 12);
                memcpy(ser + 12, g, len);
                r = ZSTD_decompress(out, 8192, ser, 12 + len);
                CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) ==
                      (len < gate ? want_short : want_long),
                      "skip+gar d=%d len=%zu: %s", d, len, ZSTD_getErrorName(r));
                free(ser);
            }
        }
    }

    /* Null / truncated buffers through the context family. */
    {
        ZSTD_DCtx *dctx = ZSTD_createDCtx();
        size_t r;
        r = ZSTD_decompressDCtx(dctx, out, 0, cmp, c);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_dstSize_tooSmall,
              "dctx dst0: %s", ZSTD_getErrorName(r));
        r = ZSTD_decompressDCtx(dctx, NULL, 100, cmp, c);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_dstSize_tooSmall,
              "dctx null dst: %s", ZSTD_getErrorName(r));
        r = ZSTD_decompressDCtx(dctx, out, 8192, NULL, 4);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_srcSize_wrong,
              "dctx null src4: %s", ZSTD_getErrorName(r));
        CHECK(ZSTD_decompressDCtx(dctx, out, 8192, NULL, 0) == 0, "dctx null src0");
        for (size_t n = 1; n <= 8; n *= 4) {
            r = ZSTD_decompressDCtx(dctx, out, 8192, cmp, n);
            CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_srcSize_wrong,
                  "dctx short src %zu: %s", n, ZSTD_getErrorName(r));
        }
        r = ZSTD_decompress_usingDict(dctx, NULL, 100, cmp, c, NULL, 0);
        CHECK(ZSTD_isError(r) && ZSTD_getErrorCode(r) == ZSTD_error_dstSize_tooSmall,
              "usingDict null dst: %s", ZSTD_getErrorName(r));
        ZSTD_freeDCtx(dctx);
    }

    /* The deprecated init family all return ZSTD_startingInputLength. */
    {
        ZSTD_DStream *zds = ZSTD_createDStream();
        CHECK(ZSTD_initDStream(zds) == 5, "initDStream hint");
        CHECK(ZSTD_initDStream_usingDict(zds, NULL, 0) == 5, "initDStream_usingDict hint");
        CHECK(ZSTD_initDStream_usingDDict(zds, NULL) == 5, "initDStream_usingDDict hint");
        CHECK(ZSTD_resetDStream(zds) == 5, "resetDStream hint");
        ZSTD_freeDStream(zds);
    }

    /* No-forward-progress: sustained mid-frame starvation errors on the
     * 16th call (inputEmpty when the output has room, destFull when it
     * has none); the between-frames state never counts (its hint return
     * sits before the reference guard); a session reset revives. */
    {
        ZSTD_DStream *zds = ZSTD_createDStream();
        ZSTD_initDStream(zds);
        ZSTD_inBuffer in = {cmp, c > 60 ? 60 : c - 1, 0};
        ZSTD_outBuffer ob = {out, 8192, 0};
        ZSTD_decompressStream(zds, &ob, &in);
        ZSTD_inBuffer ein = {NULL, 0, 0};
        size_t last = 0;
        int calls = 0;
        for (int i = 0; i < 30; i++) {
            last = ZSTD_decompressStream(zds, &ob, &ein);
            calls++;
            if (ZSTD_isError(last)) break;
        }
        CHECK(calls == 16 && ZSTD_isError(last) &&
              ZSTD_getErrorCode(last) == ZSTD_error_noForwardProgress_inputEmpty,
              "inputEmpty guard: calls=%d %s", calls, ZSTD_getErrorName(last));
        ZSTD_initDStream(zds);
        ZSTD_inBuffer in2 = {cmp, c, 0};
        ZSTD_outBuffer ob2 = {out, 8192, 0};
        size_t total = 0;
        for (;;) {
            size_t r = ZSTD_decompressStream(zds, &ob2, &in2);
            if (ZSTD_isError(r)) { CHECK(0, "revive: %s", ZSTD_getErrorName(r)); break; }
            total += ob2.pos;
            ob2.pos = 0;
            if (r == 0) break;
        }
        CHECK(total == 4096, "revive output %zu", total);
        ZSTD_freeDStream(zds);
    }
    {
        ZSTD_DStream *zds = ZSTD_createDStream();
        ZSTD_initDStream(zds);
        ZSTD_inBuffer in = {cmp, c, 0};
        ZSTD_outBuffer ob0 = {NULL, 0, 0};
        size_t last = 0;
        for (int i = 0; i < 25; i++) {
            last = ZSTD_decompressStream(zds, &ob0, &in);
            if (ZSTD_isError(last)) break;
        }
        CHECK(ZSTD_isError(last) &&
              ZSTD_getErrorCode(last) == ZSTD_error_noForwardProgress_destFull,
              "destFull guard: %s", ZSTD_getErrorName(last));
        ZSTD_freeDStream(zds);
    }
    {
        /* Post-completion calls ask for the next frame header (9) and
         * never trip the guard. */
        ZSTD_DStream *zds = ZSTD_createDStream();
        ZSTD_initDStream(zds);
        ZSTD_inBuffer in = {cmp, c, 0};
        ZSTD_outBuffer ob = {out, 8192, 0};
        size_t r = 1;
        while (r != 0 && !ZSTD_isError(r)) r = ZSTD_decompressStream(zds, &ob, &in);
        CHECK(r == 0, "completion");
        ZSTD_inBuffer ein = {NULL, 0, 0};
        ZSTD_outBuffer eb = {out, 8192, ob.pos};
        for (int i = 0; i < 25; i++) {
            size_t h = ZSTD_decompressStream(zds, &eb, &ein);
            CHECK(!ZSTD_isError(h) && h == 9, "post-done hint %zu", h);
        }
        ZSTD_freeDStream(zds);
    }
    {
        /* compressStream2 has no no-forward-progress guard. */
        ZSTD_CCtx *cs = ZSTD_createCStream();
        ZSTD_CCtx_reset(cs, ZSTD_reset_session_only);
        ZSTD_inBuffer ein = {NULL, 0, 0};
        unsigned char obuf[4096];
        for (int i = 0; i < 20; i++) {
            ZSTD_outBuffer o = {obuf, sizeof obuf, 0};
            size_t r = ZSTD_compressStream2(cs, &o, &ein, ZSTD_e_continue);
            CHECK(!ZSTD_isError(r), "cs2 starve: %s", ZSTD_getErrorName(r));
        }
        ZSTD_freeCCtx(cs);
    }

    free(cmp);
    free(out);
}


/* ---------------------------------------------------------------- -- r75 --
 * Bounds, cParams helpers, estimates, frame-walk entries, the parameter
 * readback, the prefix family and skippable payload I/O. Expectations are
 * identical on the reference and this build (the numbers below are the
 * reference's).
 */

static void test_bounds(void) {
    struct { int id; long long err; int lo, hi; } crows[] = {
        {100, 0, -131072, 22}, {101, 0, 10, 31}, {102, 0, 6, 30},
        {103, 0, 6, 30}, {104, 0, 1, 30}, {105, 0, 3, 7},
        {106, 0, 0, 131072}, {107, 0, 1, 9}, {160, 0, 0, 2},
        {161, 0, 6, 30}, {162, 0, 4, 4096}, {163, 0, 1, 8},
        {164, 0, 0, 25}, {200, 0, 0, 1}, {201, 0, 0, 1}, {202, 0, 0, 1},
        {400, 0, 0, 0}, {401, 0, 0, 0}, {402, 0, 0, 0}, {500, 0, 0, 1},
        {1000, 0, 0, 1}, {1001, 0, 0, 3}, {1002, 0, 0, 2},
        {1004, 0, 0, 2147483647}, {1005, 0, 0, 1}, {1006, 0, 0, 1},
        {1007, 0, 0, 1}, {1008, 0, 0, 1}, {1009, 0, 0, 1},
        {1010, 0, 0, 2}, {1011, 0, 0, 2}, {1012, 0, 0, 1},
        {1013, 0, 0, 2}, {1014, 0, 0, 1}, {1015, 0, 1024, 131072},
        {1016, 0, 0, 2}, {1017, 0, 0, 6},
        {99, -40, 0, 0}, {1003, -40, 0, 0}, {1018, -40, 0, 0}, {0, -40, 0, 0},
    };
    for (size_t i = 0; i < sizeof(crows) / sizeof(crows[0]); i++) {
        ZSTD_bounds b = ZSTD_cParam_getBounds((ZSTD_cParameter)crows[i].id);
        CHECK((long long)b.error == crows[i].err && b.lowerBound == crows[i].lo
              && b.upperBound == crows[i].hi,
              "cbounds %d: err=%lld lo=%d hi=%d", crows[i].id,
              (long long)b.error, b.lowerBound, b.upperBound);
    }
    struct { int id; long long err; int lo, hi; } drows[] = {
        {100, 0, 10, 31}, {1000, 0, 0, 1}, {1001, 0, 0, 1}, {1002, 0, 0, 1},
        {1003, 0, 0, 1}, {1004, 0, 0, 1}, {1005, 0, 1024, 131072},
        {99, -40, 0, 0}, {1006, -40, 0, 0},
    };
    for (size_t i = 0; i < sizeof(drows) / sizeof(drows[0]); i++) {
        ZSTD_bounds b = ZSTD_dParam_getBounds((ZSTD_dParameter)drows[i].id);
        CHECK((long long)b.error == drows[i].err && b.lowerBound == drows[i].lo
              && b.upperBound == drows[i].hi,
              "dbounds %d: err=%lld lo=%d hi=%d", drows[i].id,
              (long long)b.error, b.lowerBound, b.upperBound);
    }
}

static void test_cparams_helpers(void) {
    ZSTD_compressionParameters cp = ZSTD_getCParams(3, 0, 0);
    CHECK(cp.windowLog == 21 && cp.chainLog == 16 && cp.hashLog == 17
          && cp.searchLog == 1 && cp.minMatch == 5 && cp.targetLength == 0
          && cp.strategy == ZSTD_dfast,
          "level3 row %u/%u/%u/%u/%u/%u/%u", cp.windowLog, cp.chainLog,
          cp.hashLog, cp.searchLog, cp.minMatch, cp.targetLength,
          (unsigned)cp.strategy);
    /* 0 hint means unknown; explicit small sizes select tighter rows and
     * the adjust chain shrinks the window to the source. */
    cp = ZSTD_getCParams(3, 16 * 1024, 0);
    CHECK(cp.windowLog == 14 && cp.strategy == ZSTD_dfast, "16K row");
    cp = ZSTD_getCParams(3, 16 * 1024 + 1, 0);
    CHECK(cp.windowLog == 15 && cp.chainLog == 15 && cp.hashLog == 16
          && cp.strategy == ZSTD_dfast, "just past 16K");
    /* negative levels: row 0 plus the acceleration target length. */
    cp = ZSTD_getCParams(-7, 0, 0);
    CHECK(cp.windowLog == 19 && cp.targetLength == 7 && cp.strategy == ZSTD_fast,
          "negative row");
    cp = ZSTD_getCParams(-131072, 0, 0);
    CHECK(cp.targetLength == 131072, "clamped acceleration");
    cp = ZSTD_getCParams(100, 0, 0);
    CHECK(cp.windowLog == 27 && cp.strategy == ZSTD_btultra2, "level clamps at 22");
    /* getParams fills the default frame flags. */
    ZSTD_parameters p = ZSTD_getParams(1, 1 << 20, 4096);
    CHECK(p.fParams.contentSizeFlag == 1 && p.fParams.checksumFlag == 0
          && p.fParams.noDictIDFlag == 0, "getParams flags");
    CHECK(p.cParams.windowLog == 19 && p.cParams.minMatch == 7
          && p.cParams.strategy == ZSTD_fast, "dict grows the row size hint");
    /* adjustCParams clamps out-of-range knobs... */
    ZSTD_compressionParameters wild = {45, 3, 77, 0, 1, 999999, 12};
    ZSTD_compressionParameters adj = ZSTD_adjustCParams(wild, 1 << 17, 0);
    CHECK(adj.windowLog == 17 && adj.chainLog == 6 && adj.hashLog == 18
          && adj.searchLog == 1 && adj.minMatch == 3 && adj.targetLength == 131072
          && adj.strategy == ZSTD_btultra2,
          "adjust clamps: %u/%u/%u/%u/%u/%u/%u", adj.windowLog, adj.chainLog,
          adj.hashLog, adj.searchLog, adj.minMatch, adj.targetLength,
          (unsigned)adj.strategy);
    /* ...and checkCParams rejects them. */
    CHECK(ZSTD_isError(ZSTD_checkCParams(wild))
          && ZSTD_getErrorCode(ZSTD_checkCParams(wild)) == ZSTD_error_parameter_outOfBound,
          "checkCParams out of bound");
    CHECK(ZSTD_checkCParams(ZSTD_getCParams(12, 0, 0)) == 0, "checkCParams row ok");
    /* srcSize 0 adjusts as unknown; a known size shrinks the window. */
    adj = ZSTD_adjustCParams(ZSTD_getCParams(19, 0, 0), 8192, 0);
    CHECK(adj.windowLog == 13, "adjust shrinks to srcLog");
}

static size_t align_up_probe(size_t n) { return (n + 7) / 8 * 8; }

static void test_estimates(void) {
    CHECK(ZSTD_estimateDCtxSize() == 95976, "estimateDCtxSize %zu",
          ZSTD_estimateDCtxSize());
    CHECK(ZSTD_estimateCCtxSize(-1) == 549912, "ecc(-1) %zu", ZSTD_estimateCCtxSize(-1));
    CHECK(ZSTD_estimateCCtxSize(1) == 582680, "ecc(1) %zu", ZSTD_estimateCCtxSize(1));
    CHECK(ZSTD_estimateCCtxSize(3) == 1303576, "ecc(3) %zu", ZSTD_estimateCCtxSize(3));
    CHECK(ZSTD_estimateCCtxSize(19) == 85196950, "ecc(19) %zu", ZSTD_estimateCCtxSize(19));
    CHECK(ZSTD_estimateCCtxSize(22) == 739590294, "ecc(22) %zu", ZSTD_estimateCCtxSize(22));
    CHECK(ZSTD_estimateCCtxSize(100) == ZSTD_estimateCCtxSize(22), "ecc clamps");
    CHECK(ZSTD_estimateCStreamSize(1) == 1369625, "ecs(1) %zu", ZSTD_estimateCStreamSize(1));
    CHECK(ZSTD_estimateCStreamSize(22) == 874070679, "ecs(22) %zu", ZSTD_estimateCStreamSize(22));
    CHECK(ZSTD_estimateCStreamSize(1) > ZSTD_estimateCCtxSize(1), "stream adds buffers");
    /* via raw cParams: a hand-made row, both row-finder layouts. */
    ZSTD_compressionParameters cp = {21, 21, 22, 5, 3, 16, ZSTD_lazy2};
    CHECK(ZSTD_estimateCCtxSize_usingCParams(cp) == 26327446,
          "eccu lazy2 %zu", ZSTD_estimateCCtxSize_usingCParams(cp));
    CHECK(ZSTD_estimateCStreamSize_usingCParams(cp) == 28687255,
          "ecsu lazy2 %zu", ZSTD_estimateCStreamSize_usingCParams(cp));
    CHECK(ZSTD_estimateCStreamSize_usingCParams(cp)
          > ZSTD_estimateCCtxSize_usingCParams(cp), "stream buffers grow");
    ZSTD_compressionParameters fast = {10, 10, 17, 1, 5, 0, ZSTD_fast};
    CHECK(ZSTD_estimateCCtxSize_usingCParams(fast) == 553752,
          "eccu fast %zu", ZSTD_estimateCCtxSize_usingCParams(fast));
    /* LDM rows grow the budget through the LDM tables. */
    ZSTD_compressionParameters ultra = {27, 27, 25, 9, 3, 999, ZSTD_btultra2};
    CHECK(ZSTD_estimateCCtxSize_usingCParams(ultra) > ZSTD_estimateCCtxSize_usingCParams(fast),
          "ldm row grows");
    /* dictionary estimates: a CDict build for a known dictionary shrinks
     * its row below the unknown-dict budget. */
    CHECK(ZSTD_estimateCDictSize(0, 3) == 801344, "ecd(0,3) %zu", ZSTD_estimateCDictSize(0, 3));
    CHECK(ZSTD_estimateCDictSize(4096, 3) == 117312, "ecd(4096,3) %zu", ZSTD_estimateCDictSize(4096, 3));
    CHECK(ZSTD_estimateCDictSize_advanced(4096, fast, ZSTD_dlm_byRef) == 543296,
          "ecda byRef %zu", ZSTD_estimateCDictSize_advanced(4096, fast, ZSTD_dlm_byRef));
    CHECK(ZSTD_estimateCDictSize_advanced(4096, fast, ZSTD_dlm_byCopy) == 547392,
          "ecda byCopy %zu", ZSTD_estimateCDictSize_advanced(4096, fast, ZSTD_dlm_byCopy));
    CHECK(ZSTD_estimateDDictSize(0, ZSTD_dlm_byRef) == 27352, "edd byRef %zu",
          ZSTD_estimateDDictSize(0, ZSTD_dlm_byRef));
    CHECK(ZSTD_estimateDDictSize(1000, ZSTD_dlm_byCopy) == 28352, "edd byCopy %zu",
          ZSTD_estimateDDictSize(1000, ZSTD_dlm_byCopy));
    /* streaming decode budget */
    CHECK(ZSTD_estimateDStreamSize(0) == 96040, "eds(0) %zu", ZSTD_estimateDStreamSize(0));
    CHECK(ZSTD_estimateDStreamSize(131072) == 620328,
          "eds(128K) %zu", ZSTD_estimateDStreamSize(131072));
    /* sequence and output-buffer bounds */
    CHECK(ZSTD_sequenceBound(0) == 2 && ZSTD_sequenceBound(131072) == 43820,
          "sequenceBound");
    CHECK(ZSTD_decodingBufferSize_min(1 << 20, ZSTD_CONTENTSIZE_UNKNOWN)
          == (1 << 20) + 2 * 131072 + 64, "dbmin");
    CHECK(ZSTD_decodingBufferSize_min(1 << 20, 100) == 100, "dbmin caps at fcs");
}

static void test_frame_walk(void) {
    unsigned char *src = gen_input(70000);
    size_t cap = ZSTD_compressBound(70000);
    unsigned char *cmp = malloc(cap);
    unsigned char *out = malloc(70001);
    size_t c = ZSTD_compress(cmp, cap, src, 70000, 5);
    CHECK(!ZSTD_isError(c), "walk compress");
    /* A declared-size frame bounds to exactly its content. */
    CHECK(ZSTD_decompressBound(cmp, c) == 70000, "bound exact %llu",
          (unsigned long long)ZSTD_decompressBound(cmp, c));
    /* Margin: header + block headers + one block of slack, structural. */
    size_t margin = ZSTD_decompressionMargin(cmp, c);
    CHECK(margin >= c && margin - c < 3 * 131072 + 64, "margin shape %zu", margin);
    /* fromFrame agrees with the size-driven estimate. */
    CHECK(ZSTD_estimateDStreamSize_fromFrame(cmp, c)
          == ZSTD_estimateDStreamSize((size_t)ZSTD_getFrameContentSize(cmp, c)),
          "fromFrame");
    /* error cells: garbage, truncation, empty */
    CHECK(ZSTD_decompressBound(NULL, 0) == 0, "empty bound is 0");
    CHECK(ZSTD_decompressBound(cmp, 0) == 0, "zero-size input bounds to 0");
    unsigned char junk[32];
    for (size_t k = 0; k < 32; k++) junk[k] = (unsigned char)(k * 13 + 7);
    CHECK(ZSTD_decompressBound(junk, 32) == (unsigned long long)(0 - 2), "garbage bound");
    CHECK(ZSTD_getErrorCode(ZSTD_decompressionMargin(junk, 32))
          == ZSTD_error_prefix_unknown, "garbage margin code");
    CHECK(ZSTD_getErrorCode(ZSTD_decompressionMargin(cmp, c - 1))
          == ZSTD_error_corruption_detected, "truncated margin code");
    CHECK(ZSTD_getErrorCode(ZSTD_estimateDStreamSize_fromFrame(junk, 32))
          == ZSTD_error_prefix_unknown, "garbage fromFrame");
    CHECK(ZSTD_getErrorCode(ZSTD_estimateDStreamSize_fromFrame(cmp, 3))
          == ZSTD_error_srcSize_wrong, "short fromFrame");
    /* skippable frames skip whole through the bound walk */
    unsigned char skip[16];
    CHECK(ZSTD_writeSkippableFrame(skip, 16, src, 4, 7) == 12, "write skip");
    CHECK(ZSTD_isSkippableFrame(skip, 12) == 1 && ZSTD_isSkippableFrame(cmp, c) == 0,
          "isSkippable");
    CHECK(ZSTD_decompressBound(skip, 12) == 0, "skip bound");
    CHECK(ZSTD_decompressionMargin(skip, 12) == 12, "skip margin");
    unsigned variant = 99;
    CHECK(ZSTD_readSkippableFrame(out, 70000, &variant, skip, 12) == 4
          && variant == 7 && memcmp(out, src, 4) == 0, "read skip");
    CHECK(ZSTD_getErrorCode(ZSTD_readSkippableFrame(out, 70000, NULL, cmp, c))
          == ZSTD_error_frameParameter_unsupported, "read skip on zstd frame");
    CHECK(ZSTD_getErrorCode(ZSTD_readSkippableFrame(out, 70000, NULL, skip, 4))
          == ZSTD_error_srcSize_wrong, "read skip short");
    CHECK(ZSTD_getErrorCode(ZSTD_writeSkippableFrame(skip, 16, src, 4, 16))
          == ZSTD_error_parameter_outOfBound, "variant bound");
    CHECK(ZSTD_getErrorCode(ZSTD_writeSkippableFrame(skip, 11, src, 4, 0))
          == ZSTD_error_dstSize_tooSmall, "write capacity");
    CHECK(ZSTD_isDeterministicBuild() == 1, "deterministic build");
    free(src); free(cmp); free(out);
}

static void test_parameter_readback(void) {
    ZSTD_CCtx *cctx = ZSTD_createCCtx();
    int v = -1;
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_compressionLevel, &v) == 0 && v == 3,
          "fresh level %d", v);
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_windowLog, &v) == 0 && v == 0, "fresh wlog");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_hashLog, &v) == 0 && v == 0, "fresh hlog");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_strategy, &v) == 0 && v == 0, "fresh strat");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_enableLongDistanceMatching, &v) == 0 && v == 0,
          "fresh ldm");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_contentSizeFlag, &v) == 0 && v == 1, "fcs flag");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_checksumFlag, &v) == 0 && v == 0, "ck flag");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_dictIDFlag, &v) == 0 && v == 1, "dictid flag");
    CHECK(ZSTD_CCtx_getParameter(cctx, ZSTD_c_nbWorkers, &v) == 0 && v == 0, "workers");
    CHECK(ZSTD_getErrorCode(ZSTD_CCtx_getParameter(cctx, ZSTD_c_jobSize, &v))
          == ZSTD_error_parameter_unsupported, "jobSize unsupported");
    CHECK(ZSTD_getErrorCode(ZSTD_CCtx_getParameter(cctx, (ZSTD_cParameter)12345, &v))
          == ZSTD_error_parameter_unsupported, "unknown id");
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, 9);
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_windowLog, 20);
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_enableLongDistanceMatching, 1);
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 0);
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, 1);
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_dictIDFlag, 0);
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_compressionLevel, &v);
    CHECK(v == 9, "level readback %d", v);
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_windowLog, &v);
    CHECK(v == 20, "wlog readback %d", v);
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_enableLongDistanceMatching, &v);
    CHECK(v == 1, "ldm readback %d", v);
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_contentSizeFlag, &v);
    CHECK(v == 0, "fcs readback");
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_checksumFlag, &v);
    CHECK(v == 1, "ck readback");
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_dictIDFlag, &v);
    CHECK(v == 0, "dictid readback");
    /* level 0 stores the default */
    ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, 0);
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_compressionLevel, &v);
    CHECK(v == 3, "level 0 readback %d", v);
    /* parameters reset returns the defaults */
    ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters);
    ZSTD_CCtx_getParameter(cctx, ZSTD_c_windowLog, &v);
    CHECK(v == 0, "reset readback");
    /* getBlockSize: 0 fresh, then the applied frame's shape */
    CHECK(ZSTD_getBlockSize(cctx) == 0, "fresh getBlockSize");
    ZSTD_freeCCtx(cctx);

    cctx = ZSTD_createCCtx();
    static unsigned char buf[1 << 21];
    for (size_t i = 0; i < sizeof(buf); i++) buf[i] = (unsigned char)('a' + (i % 23));
    static unsigned char cmpbuf[4 << 20];
    ZSTD_compress2(cctx, cmpbuf, sizeof(cmpbuf), buf, 100);
    CHECK(ZSTD_getBlockSize(cctx) == 1024, "tiny frame block %zu", ZSTD_getBlockSize(cctx));
    ZSTD_compress2(cctx, cmpbuf, sizeof(cmpbuf), buf, 4096);
    CHECK(ZSTD_getBlockSize(cctx) == 4096, "4K frame block %zu", ZSTD_getBlockSize(cctx));
    ZSTD_compress2(cctx, cmpbuf, sizeof(cmpbuf), buf, 1 << 20);
    CHECK(ZSTD_getBlockSize(cctx) == 131072, "1M frame block %zu", ZSTD_getBlockSize(cctx));
    /* survives a session reset like appliedParams */
    ZSTD_CCtx_reset(cctx, ZSTD_reset_session_only);
    CHECK(ZSTD_getBlockSize(cctx) == 131072, "post-reset block");
    ZSTD_freeCCtx(cctx);

    ZSTD_DCtx *dctx = ZSTD_createDCtx();
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_windowLogMax, &v) == 0 && v == 27,
          "fresh dwindow %d", v);
    ZSTD_DCtx_setParameter(dctx, ZSTD_d_windowLogMax, 24);
    ZSTD_DCtx_getParameter(dctx, ZSTD_d_windowLogMax, &v);
    CHECK(v == 24, "dwindow readback %d", v);
    /* The other dParameters read back their never-changed defaults. */
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_format, &v) == 0 && v == 0, "dformat %d", v);
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_stableOutBuffer, &v) == 0 && v == 0,
          "dstable %d", v);
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_forceIgnoreChecksum, &v) == 0 && v == 0,
          "dchecksum %d", v);
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_refMultipleDDicts, &v) == 0 && v == 0,
          "dmulti %d", v);
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_disableHuffmanAssembly, &v) == 0 && v == 0,
          "dhuf %d", v);
    CHECK(ZSTD_DCtx_getParameter(dctx, ZSTD_d_maxBlockSize, &v) == 0 && v == 0,
          "dmaxblock %d", v);
    CHECK(ZSTD_getErrorCode(ZSTD_DCtx_getParameter(dctx, (ZSTD_dParameter)1006, &v))
          == ZSTD_error_parameter_unsupported, "dparam unknown");
    ZSTD_freeDCtx(dctx);
}

static void test_prefix_family(void) {
    static unsigned char buf[1 << 21];
    for (size_t i = 0; i < sizeof(buf); i++) buf[i] = (unsigned char)('a' + (i % 23));
    static unsigned char cmpbuf[4 << 20];
    static unsigned char out[1 << 21];
    size_t cap = sizeof(cmpbuf);

    /* compress the tail with the head as prefix; decode needs the prefix. */
    ZSTD_CCtx *cctx = ZSTD_createCCtx();
    ZSTD_CCtx_refPrefix(cctx, buf, 1 << 18);
    size_t c1 = ZSTD_compress2(cctx, cmpbuf, cap, buf + (1 << 18), 1 << 18);
    CHECK(!ZSTD_isError(c1), "prefix compress2 %zu", c1);
    /* the prefix is single-use: the next frame is byte-identical to a
     * fresh-context compression of the same bytes. */
    size_t c2 = ZSTD_compress2(cctx, cmpbuf + c1, cap - c1, buf + (1 << 18), 1 << 18);
    CHECK(!ZSTD_isError(c2), "second frame");
    ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters);
    size_t c2_plain = ZSTD_compress2(cctx, out, 1 << 20, buf + (1 << 18), 1 << 18);
    CHECK(c2 == c2_plain && memcmp(cmpbuf + c1, out, c2) == 0,
          "single-use prefix lingers (%zu vs %zu)", c2, c2_plain);

    ZSTD_DCtx *dctx = ZSTD_createDCtx();
    ZSTD_DCtx_refPrefix(dctx, buf, 1 << 18);
    size_t d = ZSTD_decompressDCtx(dctx, out, 1 << 18, cmpbuf, c1);
    CHECK(d == (1 << 18) && memcmp(out, buf + (1 << 18), 1 << 18) == 0, "prefix decode");
    /* the decode prefix is consumed too: re-decoding the same frame now
     * fails exactly as the reference's use_once dictates. */
    d = ZSTD_decompressDCtx(dctx, out, 1 << 18, cmpbuf, c1);
    CHECK(ZSTD_isError(d), "use_once second decode unexpectedly ok");
    /* a fresh prefix makes it decodable again */
    ZSTD_DCtx_refPrefix(dctx, buf, 1 << 18);
    d = ZSTD_decompressDCtx(dctx, out, 1 << 18, cmpbuf, c1);
    CHECK(!ZSTD_isError(d) && memcmp(out, buf + (1 << 18), 1 << 18) == 0, "re-armed prefix");
    /* the plain second frame decodes without any prefix */
    d = ZSTD_decompressDCtx(dctx, out, 1 << 18, cmpbuf + c1, c2);
    CHECK(!ZSTD_isError(d) && memcmp(out, buf + (1 << 18), 1 << 18) == 0, "plain frame");

    /* refPrefix mid-frame is stage_wrong */
    ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters);
    ZSTD_outBuffer ob = {out, 1 << 20, 0};
    ZSTD_inBuffer ib = {buf, 1 << 18, 0};
    ZSTD_compressStream2(cctx, &ob, &ib, ZSTD_e_continue);
    CHECK(ZSTD_getErrorCode(ZSTD_CCtx_refPrefix(cctx, buf, 4096))
          == ZSTD_error_stage_wrong, "mid-frame refPrefix");
    ZSTD_inBuffer ib2 = {buf, 0, 0};
    ZSTD_compressStream2(cctx, &ob, &ib2, ZSTD_e_end);

    /* advanced content types: fullDict on plain content is an error at
     * frame time; rawContent rides the forced-raw load. */
    /* (into `out`: cmpbuf[0..c1) must survive for the decode checks) */
    ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters);
    ZSTD_CCtx_loadDictionary_advanced(cctx, buf, 4096, ZSTD_dlm_byCopy, ZSTD_dct_fullDict);
    size_t r = ZSTD_compress2(cctx, out, 1 << 20, buf, 1000);
    CHECK(ZSTD_isError(r), "fullDict plain errors");
    ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters);
    ZSTD_CCtx_loadDictionary_byReference(cctx, buf, 4096);
    r = ZSTD_compress2(cctx, out, 1 << 20, buf, 1000);
    CHECK(!ZSTD_isError(r), "byReference load compresses");

    /* refDDict attach and the streaming prefix path */
    ZSTD_DDict *ddict = ZSTD_createDDict(buf, 1 << 18);
    ZSTD_DCtx_reset(dctx, ZSTD_reset_session_and_parameters);
    ZSTD_DCtx_refDDict(dctx, ddict);
    d = ZSTD_decompressDCtx(dctx, out, 1 << 18, cmpbuf, c1);
    CHECK(!ZSTD_isError(d) && memcmp(out, buf + (1 << 18), 1 << 18) == 0, "refDDict decode");
    /* refDDict(NULL) clears */
    ZSTD_DCtx_refDDict(dctx, NULL);
    CHECK(ZSTD_getErrorCode(ZSTD_DCtx_refDDict(dctx, NULL)) == 0, "clear ok");
    ZSTD_freeDDict(ddict);

    /* streaming decode with a prefix across a two-frame stream */
    ZSTD_DCtx_reset(dctx, ZSTD_reset_session_and_parameters);
    ZSTD_DCtx_refPrefix(dctx, buf, 1 << 18);
    size_t both = c1; /* frame 1 needs the prefix, frame 2 is plain */
    ZSTD_inBuffer dib = {cmpbuf, both, 0};
    ZSTD_outBuffer dob = {out, 1 << 20, 0};
    for (int i = 0; i < 8 && dib.pos < dib.size; i++) {
        size_t h = ZSTD_decompressStream(dctx, &dob, &dib);
        CHECK(!ZSTD_isError(h), "stream prefix decode %s", ZSTD_getErrorName(h));
        if (ZSTD_isError(h)) break;
    }
    CHECK(dib.pos == both && dob.pos == (1 << 18), "stream prefix drained in=%zu out=%zu",
          dib.pos, dob.pos);
    ZSTD_freeCCtx(cctx);
    ZSTD_freeDCtx(dctx);
}

int main(int argc, char **argv) {
    if (argc == 4 && strcmp(argv[1], "produce") == 0)
        return produce(argv[2], argv[3], 3, 0);
    if (argc == 5 && strcmp(argv[1], "produce-stream") == 0)
        return produce(argv[2], argv[3], atoi(argv[4]), 1);
    if (argc == 4 && strcmp(argv[1], "consume") == 0)
        return consume(argv[2], argv[3]);
    if (argc != 1) { printf("usage: harness | produce <raw> <zst> | produce-stream <raw> <zst> <level> | consume <zst> <raw>\n"); return 2; }

    test_version();
    test_one_shot();
    test_errors();
    test_contexts();
    test_streaming();
    test_reset_compress2();
    test_dicts();
    test_dctx_reset_and_metadata();
    test_r67_round2();
    test_bounds();
    test_cparams_helpers();
    test_estimates();
    test_frame_walk();
    test_parameter_readback();
    test_prefix_family();
    printf(failures ? "SELF-TESTS FAILED (%d)\n" : "self-tests OK (%d failures)\n", failures);
    return failures ? 1 : 0;
}
