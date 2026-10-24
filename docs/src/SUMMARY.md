# Summary

[Introduction](intro.md)

---

# For Users

- [User guide](user.md)

---

# Development Records

- [Status overview](dev/status.md)

## Performance Comparisons

- [Performance snapshot](dev/bench/snapshot.md)
- [Bench matrix](dev/bench/matrix.md)
- [Benchmark methodology](dev/bench/methodology.md)
- [Decode stream gap against libzstd](dev/bench/dec-gap.md)
- [Encode dll100 gap against libzstd](dev/bench/enc-dll-gap.md)

## Upstream Comparison

- [Comparison with official zstd](dev/comparison.md)
- [C FFI](dev/ffi.md)

## Performance Work

- [Decoding path](dev/perf/decoding.md)
- [Encoding path](dev/perf/encoding.md)
- [Matchers and compression levels](dev/perf/matchers.md)
- [Multithreading and streaming](dev/perf/mt-stream.md)

## Review Records

- [FSE core review](dev/review/fse.md)
- [Huffman coder review](dev/review/huff0.md)
- [Block layer review](dev/review/blocks.md)
- [Decode error paths](dev/review/parity.md)
- [Decode core cleanups](dev/review/declean.md)
- [Decode parity differential](dev/review/diffbench.md)
- [Dictionary-frame decode parity](dev/review/dictparity.md)
- [FFI parity](dev/review/ffiparity.md)
- [CLI parity](dev/review/cliparity.md)
- [Encoder edges](dev/review/encparity.md)
- [Encode-path hardening](dev/review/hardening.md)
- [Optimal parser review](dev/review/opt.md)
- [Window semantics](dev/review/winsem.md)

## Remaining Improvements

- [Remaining improvements](dev/todo.md)
- [Falsified directions](dev/negative.md)
  - [Decoding](dev/negative/decoding.md)
  - [Encoding matchers](dev/negative/matchers.md)
  - [Entropy, checksum and misc](dev/negative/entropy.md)
  - [Dictionary trainer](dev/negative/dict.md)
  - [MT and streaming](dev/negative/mt-stream.md)

## Pitfalls

- [Decoding pitfalls](dev/pitfalls/decoding.md)
- [Encoding pitfalls](dev/pitfalls/encoding.md)
- [Workflow pitfalls](dev/pitfalls/workflow.md)
