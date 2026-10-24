# Summary

[Introduction](intro.md)

---

# For Users

- [User Guide](user.md)

---

# Development Records

- [Current Status](dev/status.md)

## Performance Comparisons

- [Current Snapshot](dev/bench/snapshot.md)
- [Wide Matrix Data](dev/bench/matrix.md)
- [Benchmark Methodology](dev/bench/methodology.md)
- [Decode Gap vs libzstd — Differential Attribution](dev/bench/dec-gap.md)
- [Encode dll100 Gap vs libzstd — Differential Attribution](dev/bench/enc-dll-gap.md)

## Upstream Comparison

- [zstdx vs. official zstd](dev/comparison.md)
- [C FFI (libzstd compatibility layer)](dev/ffi.md)

## Review Rounds

- [R64 · FSE core](dev/review/fse.md)
- [R64 · Huff0 + bit I/O](dev/review/huff0.md)
- [R64 · Blocks + xxh64](dev/review/blocks.md)
- [R65 · Decode error-path parity](dev/review/parity.md)
- [R65 · Decode cleanups](dev/review/declean.md)
- [R66 · Parity bench mode](dev/review/diffbench.md)
- [R66 · Dictionary parity](dev/review/dictparity.md)
- [R67 · FFI parity](dev/review/ffiparity.md)
- [R67 · CLI parity](dev/review/cliparity.md)
- [R68 · Encoder edges](dev/review/encparity.md)
- [R69 · Encode-path hardening](dev/review/hardening.md)
- [R70 · Optimal parser](dev/review/opt.md)
- [R72 · Window semantics](dev/review/winsem.md)

## Performance Optimization

- [Decoding Side](dev/perf/decoding.md)
- [Encoding Side](dev/perf/encoding.md)
- [Matchers and Compression Levels](dev/perf/matchers.md)
- [Multithreading and Streaming](dev/perf/mt-stream.md)

## Remaining Improvements

- [Todo List](dev/todo.md)
- [Disproven Directions (Do Not Retry)](dev/negative.md)
  - [Decoding](dev/negative/decoding.md)
  - [Encoding · Matchers](dev/negative/matchers.md)
  - [Encoding · Entropy / Checksum / Misc](dev/negative/entropy.md)
  - [Dictionary Trainer](dev/negative/dict.md)
  - [MT / Streaming](dev/negative/mt-stream.md)

## Pitfalls

- [Decoding Side](dev/pitfalls/decoding.md)
- [Encoding Side](dev/pitfalls/encoding.md)
- [Engineering and Benchmark Methodology](dev/pitfalls/workflow.md)
