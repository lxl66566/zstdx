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

## Upstream Comparison

- [zstdx vs. official zstd](dev/comparison.md)

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
  - [MT / Streaming](dev/negative/mt-stream.md)

## Pitfalls

- [Decoding Side](dev/pitfalls/decoding.md)
- [Encoding Side](dev/pitfalls/encoding.md)
- [Engineering and Benchmark Methodology](dev/pitfalls/workflow.md)
