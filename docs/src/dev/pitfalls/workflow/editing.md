## fsmonitor swallows tool writes


After a Python script or cargo fmt modifies files, the fsmonitor daemon does not report it, so git believes the files are unchanged while the blobs differ. Status shows clean, and a pathspec commit silently commits old content. This once lost all source changes in a fix commit, exposed only when `git show --stat`'s file count did not add up.

In this repository run all git commands as `git -c core.fsmonitor=false`, and always check the stat file count after committing. A knock-on effect is that a pathspec commit resets the index to HEAD plus the committed paths, so a previously staged `git mv` rename falls out of the index entirely. Files ignored by .gitignore but historically force-tracked, such as fuzz artifacts, must be re-staged with `git add -f` or the rename is committed as a pure deletion.
## rustfmt rewrites the escaped string continuation


The installed rustfmt builds (2026-08 and 2026-09 nightlies, with this repo's `format_strings = true`) rewrap `decode_mt.rs`'s escaped line-continuation string in the `records` builder by moving text across the continuation boundary and emitting a doubled backslash, which changes the string and fails to compile. `cargo fmt --check` on the committed tree wants that rewrite and a width rewrap of one match arm, so the file is not fmt-clean on this box and `cargo fmt` in `crates/zstdx-fuzz` must not run over it. The fuzz crate is not a workspace member, so a root-level `cargo fmt --all` leaves it alone; format `encode.rs` and `encode_stream.rs` per file and keep `decode_mt.rs` hand-formatted to the committed style until rustfmt's continuation rewrite is fixed.
# Editing tools


- The Edit tool matches by text. Identical tail context in the X1 and X2 functions once caused edits to the wrong function, so when editing similar functions the old string must carry an identifier unique to each.
- Match-arm edits should contain only the target arm. One edit copied the next arm wholesale into the new string and produced a duplicate arm exposed only by clippy's unreachable-pattern lint, with behavior that happened to stay correct but redundant.
## Never git-checkout a file carrying uncommitted work


`git checkout -- <path>` restores the index copy, so in a fresh worktree it silently discards uncommitted implementation work. Take a /tmp snapshot of every modified file before any restore, verify it by hash, and restore working files from the snapshot instead of the index; keep it until the work is committed.
## Verify batch restores with git diff


A Python replace missed one site because `replace(pat, 2)` hits only the first two, and json.fastest size drifted by 101 B, caught only through deterministic size comparison. Diff first after every change, because deterministic sizes are a free regression probe.
## Keep /tmp headroom before a ratio sweep


`ratio --parallel 16` transiently holds several 32 MiB buffers per cell on both sides; on a 13 GB box with dump directories still in the /tmp RAMDisk the sweep was SIGTERM'd at cell 105 of 120, and stdout is block-buffered so the completed rows were lost with it. Diff and delete dump directories right after the gate, and drop to `--parallel 8` when free RAM is under half the total.
## Share payloads across parallel agents on the /tmp RAMDisk


Two runtime OOM kills in one round came from parallel worktree agents each materializing private dll-class payload copies on /tmp (a 400 MiB file per agent) beside their dump-gate trees, pushing the tmpfs past 80 percent with swap exhausted. Agents probing the same payload hardlink one canonical copy into their own /tmp directories instead of copying, dump trees are deleted the moment their cmp passes, and a cleanup pass between arms keeps the RAMDisk under half.
