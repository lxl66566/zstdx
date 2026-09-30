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
    CHECK(ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_nbWorkers, 2)) ||
          ZSTD_CCtx_setParameter(cctx, ZSTD_c_nbWorkers, 2) == 2, "workers param");
    CHECK(ZSTD_CCtx_setParameter(cctx, ZSTD_c_windowLog, 18) == 18, "window param");
    CHECK(ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_windowLog, 99)), "window oob");
    /* hashLog: this layer models a parameter subset; libzstd accepts it. */
    CHECK(ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_hashLog, 17)) ||
          ZSTD_CCtx_setParameter(cctx, ZSTD_c_hashLog, 17) == 17, "hashLog");

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
    }

    ZSTD_freeDCtx(dctx);
    free(src);
    free(cmp);
    free(out);
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
    printf(failures ? "SELF-TESTS FAILED (%d)\n" : "self-tests OK (%d failures)\n", failures);
    return failures ? 1 : 0;
}
