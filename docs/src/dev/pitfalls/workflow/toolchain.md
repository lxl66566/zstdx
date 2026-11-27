# Toolchain and environment
## Python frame parsing


In `(b1<<8)|b2+0x7f00`, `+` binds tighter than `|`. The literals-header size-field bit width is easy to misremember. `sort -rn` mixed in kernel symbols. Check an analysis tool's field definitions against a known-correct decoder before writing it.

- Background task output should not go through tail or head in pipes. Persist the full output to disk, then grep.
- Gungraun comparisons need explicit baselines. Pass `--save-baseline=X` and `--baseline=X`, because the default "No change" compares the run against itself. Baseline names reject `-`, so use `_`.
## Lane counts in gated tests


The 2026-10-24 first run of the paranoid profile caught `parallel_fill_matches_serial` invoking `fill_parallel`, whose entry `debug_assert!(lanes > 1)` gated on `parallel_fill_lanes()`, which is 3 only at `available_parallelism() >= 6` and 1 otherwise. The 4-vCPU GitHub runner failed the assert because the paranoid profile keeps debug assertions, while every 6-core-or-larger dev box passed.

`std::thread::scope` needs runnable threads, so a core count alone is insufficient for the parallel path. The lane count parameter makes the path fully exercisable on a small box, and machine thresholds stay in the production dispatch only.
## 32-bit cross targets catch two whole-crate patterns


The 2026-10-26 i686 cross job caught an address-layout literal that only fits a 64-bit `usize` (`hugepage`'s `HINT_BASE`, a hard `overflowing_literals` error), and a helper compiled everywhere whose callers all sit in `x86_64` cfg blocks (`common::simd`, dead-code warnings). Address-layout constants need one cfg arm per pointer width, and a helper module's cfg must match its narrowest caller, because the host build never sees either problem.

The same job's 190 test failures all traced to one idiom: `product_u64 as usize >> (64 - log)`. On a 32-bit target the cast truncates the product first and every `64 - log` shift then crosses the type width, so the paranoid overflow checks panic in every search loop. The shift belongs in the u64 domain with the cast after it; lookalikes that shift in the u32 or u64 domain before casting (`opt`, `pre_split`, the gate probe) are width-independent and stay.
## Running the i686 suite locally without cross


`cross` needs a container runtime; a plain local loop needs only zig and the nix 32-bit glibc. Build with a wrapper as `CC_i686_unknown_linux_gnu` and `CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER` that strips `-m32` and `--target=…` and execs `zig cc -target x86-linux-gnu` (zig rejects the `i686` architecture name). zig's synthesized glibc links but cannot run — use the real loader from `pkgsi686Linux.glibc` as `CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER="<store-path>/lib/ld-linux.so.2 --library-path <store-path>/lib"`. Run with `--test-threads=1`: parallel encoder-window tests overflow the 3 GiB address space and the failed allocation aborts the whole process.
## Running the s390x suite under qemu-user


The big-endian loop reuses the zig wrapper pattern with `-target s390x-linux-gnu`, and zig's synthesized glibc segfaults under qemu-user even when it links, so the sysroot must be the real s390x `libc6` deb from Ubuntu ports, unpacked under its own `usr/lib/s390x-linux-gnu` layout and handed to the runner as `qemu-s390x -L <sysroot>`. qemu-user executes the suite's threads correctly, so the default test parallelism stands and the full lib suite drops from 362 s serial to 88 s; the i686 single-thread rule is about that target's 3 GiB address space, not about emulation.

The 2026-11-02 run also confirmed the pipe trap's cost: a compile whose errors were piped through `rg` left `$?` reporting the filter's status, the stale host-endian binary then ran and produced a wrong conclusion, and the expensive rebuild had to be redone. After every cross build, check the test binary's mtime before running it.
## Members cannot override an inherited dep's default-features


A member dependency `{ workspace = true, default-features = false, ... }` is a hard error on newer cargo ("`default-features = false` cannot override workspace's `default-features`"), while older nightlies still accept it, so the host build stays green until CI's stable toolchain moves. List the wanted features explicitly instead: when the list already equals the workspace defaults, dropping the flag changes nothing in feature resolution.
## Reference ABI numbers move with the build target


The 2026-11-02 ffi-c run failed all ten compress-side estimate checks against the reference with every value exactly 8 bytes below the pinned expectation, while the same suite passed on the dev host and every decoder-side estimate matched on both. The harness expectations and the ffi size constants embed the reference's `sizeof(ZSTD_CCtx)`, which depends on the build target: a default of x86-64-v3 or newer defines `__BMI2__`, `DYNAMIC_BMI2` turns off, the CCtx `bmi2` field disappears and the struct shrinks by 8. The ubuntu-26.04 runner's gcc defaults to a BMI2-capable baseline, where the dev host's gcc still defaults to plain x86-64. `ZSTD_DCtx` carries the same gated field, but its padding absorbs the loss, which is why `estimateDCtxSize` and the dictionary estimates matched everywhere.

`tests/run-c.sh` therefore builds the reference static lib itself, from the pinned tree at `MOREFLAGS=-march=x86-64`, and the workflow no longer builds a ref lib step. Trusting a prebuilt `libzstd.a` couples the parity gate to whatever the local toolchain's default target happens to be, and the failure then reads as an expectations drift instead of a configuration mismatch.
## Kernel sweep tests must gate on host detection


The 2026-11-05 and 2026-11-07 workspace runs died about half a second into the zstdx lib suite with `signal: 4, SIGILL: illegal instruction` and no failing test named. `nt_kernels_reproduce_bytes_individually` swept every compiled NT-copy kernel directly, `nt_avx512` included, with no `is_x86_feature_detected!` gate: the AVX-512 dev host executed it fine, while the fleet's AMD EPYC runners (x86-64-v3) trap on the first EVEX instruction. The mixed runner fleet makes it a coin flip per run, and a SIGILL reads as a process crash, so the log shows only the last completed tests around the alphabetical frontier.

A sweep that calls a `#[target_feature]` kernel directly must gate each kernel on the same runtime detection the dispatch uses; a tier the host lacks stays covered by the hosts that report it. Reproduce locally without dedicated hardware via `qemu-x86_64 -cpu EPYC-Milan <test-binary> <filter>`, which replays the exact SIGILL.
## Tests reading a process-global switch must hold its serializer


The 2026-11-07 paranoid run failed `opt_feed_arming_caps_at_the_buffer` on the assertion that the feed engages, while every dev box passed. The verdict reads `OPT_FEED_ARMED`, a process-global switch that `opt_serial_ldm_feed_pledged_stream_matches_bulk` stands down and back up under `FEED_TEST_SWITCH` while compressing a 17 MiB fixture. The arming test took no lock, and on the 4-vCPU runner the switch-off window covers the quick arming write, so the resolve read the stood-down state.

Any test whose expectation depends on a test-global's value takes the same mutex the mutating tests use, for its whole body. The mutex exists precisely because the default harness runs tests in parallel threads of one process.
## cfg(unix) stubs silently degrade Windows behavior


`dict_input_is_refused` failed only on the Windows runner, exiting 0 where 1 was owed: the dict=input identity check lived behind `#[cfg(unix)]` as `(dev, ino)` while the `#[cfg(not(unix))]` twin answered `None`, so on Windows the guard degraded to plain path comparison and a hard-linked input passed through. The fallback compiles green on Linux, so local runs can never expose it. CI's nextest fails fast, so the 186 tests after the failure had never run on Windows either, and the whole tail of the suite must be treated as unverified when hunting the next one.

Windows has no `(dev, ino)`; its stat identity is the `BY_HANDLE_FILE_INFORMATION` pair of volume serial number plus 64-bit file index, which only exists on handle-backed metadata. Current std's `fs::metadata` does open the path with backup semantics and calls `GetFileInformationByHandle`, but the std accessors for those two fields are unstable (`windows_by_handle`), so the CLI opens the handle itself (`OpenOptions::access_mode(0)` plus `FILE_FLAG_BACKUP_SEMANTICS`, the same open std's metadata does) and reads the struct through windows-sys. The reference zstd 1.6.0 itself only compares names on Windows, so the identity check there goes beyond upstream to satisfy the shared test suite. `cargo check -p zstdx-cli --all-targets --target x86_64-pc-windows-msvc` compiles the whole graph, which is C-free, and serves as the pre-CI gate for `cfg(windows)` arms.

The same sweep caught a second instance: `strerror` printed Rust's Display text, which on Windows is FormatMessageW phrasing ("The system cannot find the file specified.") while the reference prints CRT strerror(3) texts that agree across glibc and MSVC. Mapping io::ErrorKind back onto the CRT vocabulary keeps the diagnostics byte-identical everywhere, and three of the never-run tests assert exactly those literals.
## A literal inBuffer size past its buffer faults by heap layout


`load_dictionary_stage_and_stickiness` aborted on the Windows runner with 0xc0000005 at the first `ZSTD_compressStream2` call, and passed on the dev box and on Linux. The test declared `zin.size = 8192` over a 5096-byte payload; the streaming core ingests the whole declared size (block buffering reads everything before output pressure applies), so the call read 3 KB past the Vec. Whether that lands in mapped memory is pure allocator layout, which is why the same binary faults on one heap and survives on another — a green local run proves nothing for an overread. Every `ZSTD_inBuffer`/`ZSTD_outBuffer` site carries a size derived from its backing buffer (`payload.len().min(chunk)`), and a size literal is worth auditing whenever a test written on one platform first runs on another.
