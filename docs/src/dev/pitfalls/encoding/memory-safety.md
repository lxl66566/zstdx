# Memory safety
## Reserve against len, not pos


In bulk bit-write loops `pos` often runs ahead of `len` because `set_len` is deferred to the end. `reserve(fixed value)` becomes a no-op once capacity suffices, and later stores run out of bounds and corrupt heap metadata, which explodes later as `realloc(): invalid next size`. Use `reserve(pos + N - len)`.
## Tail guards move with payload width


After the hash payload became u64, the scan tail guard `block_end - pos < 8` and the insert upper bound `len - 8` had to be tightened together to avoid reading the uninitialized tail left by `set_len`.
## Catch-up fill tail margin


`catch_up_insertions` bounded its inserts at `block_start`, the gated block's end, but each insert hashes HASH_READ bytes and the window guarantees coverage only up to that end. A gate hit followed by a sub-8-byte block therefore read past the window. On the bulk and MT paths the window borrows the caller's slice, so it is a real heap overread, which ASAN reports as a read of size 8 right past the input region, while streaming read stale capacity bytes.

Every other fill path already stopped at `len - HASH_READ`, and the bound is now `(block_start - win_base).saturating_sub(HASH_READ)`, one position short of a scan's own last insertion. Output was always valid because garbage entries only steer probes into byte-verified misses, and the trigger needs gate-then-tiny-block sequencing, which is why ordinary roundtrips never saw it.
## Inclusive bounds and empty ranges


`insert_max` is the last insertable position and is inclusive, but `insert_covered` used it as an exclusive loop end and then computed `(end - p) & 1` for the odd-position peel. A match starting past the bound leaves the range empty, the u64 subtraction wraps, and an odd wrapped difference fires the peel, which hashes 8 bytes past the window.

There are two live trigger paths. First blocks below 9 B saturate `insert_max` to `win_base`, and `rep1_chain` tails on ordinary inputs can start up to 4 past the bound because its scan bar is MIN_MATCH of `block_end`. The streaming ring buffer swallows those 1 to 4-byte overreads, and only bulk inputs whose window ends at the allocation end are ASAN-visible, which is why the fuzzer needed an 8-byte reproducer. Release stays silent and output stays valid because the garbage table entry only steers later probes.

The fix-shape A/B measured a guard branch and a `.max(p)` cmov at +0.23% Ir on json.fastest and +0.28% on text, while a subtraction-free peel-after restructure cost +0.54% because the `p + 1 < end` compare does not strength-reduce. Ship the guard, since wall is flat on the branch and latency-bound tier.
## Head inserts carry links


The chain walk reads a candidate's chain slot after resolving it through the head table, so a head insert that skips the link, such as `insert_covered` through `TableEmit::emit` for the rep1 continuation's covered fill, parks a position whose slot still holds whatever a previous job wrote. The prefill documentation's "stale link slots are unreachable" induction holds only if every head insert also links.

The link-less form let a pooled stream-mt worker's job B walk terminate at a slot left by its own job A, which made the frame bytes depend on worker-claim order, about ±25 B on a 1.1 MiB job, run to run nondeterministic and invisible to fresh-process dumps. Bulk MT was affected in principle and only looked stable through fixed claim order.

The fix is `insert_linked_at`: the chain strategies link every covered head insert through `TableEmit::emit_linked`, while the fast strategy's fills stay link-less because it has no chain. The linked fills also improve the chain band, with levels 5-12 shrinking 0.03% to 0.19% on json and text because covered positions become walkable candidates. The regression test is `pooled_state_reuse_is_output_neutral`. Any "stale X is unreachable" proof must be re-validated per insert path, not only per table, because the induction was proven for the prefill grid and assumed for the emit path.
## Exclude distance zero


A stale slot can hold `pack_pos` of the current scan position, because pooled tables reuse residue across frames and an old-cycle slot can alias anything. `dist = pos - entry + 1` then degenerates to 0, `dist <= reach` admits it, and the candidate byte-compares against itself, so `read4` and `extend` trivially match, the walk returns a zero-offset candidate, and the store gate's `(idx - cand).ilog2()` panics.

The old `unpack_pos` wrap check, which stepped one 4 GiB cycle back when `cand >= pos`, rejected this implicitly. The unsigned form `dist.wrapping_sub(1) < reach` admits exactly [1, reach] and restores it at no extra cost, and the subtraction must be wrapping too, because plain `dist - 1` panics in debug exactly at the dist-0 case it exists to reject. Release wraps, so every dump and ratio gate passed while `cargo test` failed.

The bug surfaced only in the interleaved matrix's timing loop with pooled state across rounds, never in fresh-process dumps, so pool residue is a real input dimension that the dump gate cannot see.
## Origin-shifted sentinels


The origin-free distance resolve rejected a zeroed slot for free, since `dist = pos - entry + 1` in u64 always exceeds the reach when `entry = 0`. The direct origin form computes `dist = q - entry` with `q = pos + origin + 1` in the u32 entry domain, and once the origin's accumulated advance crosses a 2^32 boundary, `q` wraps into `[0, reach]`. An empty slot then decodes as a phantom candidate for a whole reach-wide band of scan positions, silently and origin-value-dependently, fresh zeroed tables included, which is the very state a real clear used to guarantee.

Two fixes were measured. An explicit compare and select cost +3.94% Ir on json.balanced through the walk's per-step cmov, and never storing zero in links cost +12.2% through a write-side branch per insert, which is worse.

The landed form keeps the resolve in the u64 domain and caps the origin at 2^30 through a once-per-about-1-GiB real clear, so `q < 2^32` holds, live entries resolve exactly, and the sentinel dies by arithmetic again at the origin-free per-step cost. The cap-clear later became a 3.5-GiB epoch roll with a negated-floor origin, which keeps `q` small the same way without the clear, as described in [matchers](../../perf/matchers.md). Arithmetic that used to make the sentinel dead by construction must be re-derived when the coordinate domain gains a shift or a modulus, and the domain choice itself is a hot-loop lever worth an A/B before the check is written.
## Search exit contracts


`dubt::find_best`'s rep early exit returned without inserting the searched position and with `next_update` still at it. The next `fill_to` then performed the insertion, because the `pos >= next_update` guard inserts before searching, but the exit contract had diverged and a long rep match's insertion was deferred.

The fix makes the early exit write the fill-form entry of head, chain link and unsorted mark, and set `next_update = max(next_update, pos + 1)`. That is table-state equivalent to the deferred fill, but not output-equivalent, because the LDM skip consumes `next_update`'s exact value and the shifted value can flip that heuristic on rare shapes and move the frame bytes while keeping them legal. A multi-exit search owns one postcondition: the searched position is inserted and `next_update` is past it. Each new early exit must restate it instead of relying on the next fill.
## Row matcher twin contracts


`RowEmit::emit`'s covered fill wraps exactly like `insert_covered`'s old bug, where a match starting inside the final HASH_READ span meets an `insert_max` already behind it, `end_abs - from` wraps, and the peel runs about 96 out-of-window hash reads in release. `rep1_row`'s tail compares a fourth byte past the window at exactly MIN_MATCH bytes of block left, the `rep1_chain` twin, and row-inserts the probe, where `row_hash_at` reads the full 8-byte HASH_READ past the buffer.

Two of the three fuzz findings in that round are the row twins of recorded chain pitfalls. Any fill, emit or tail change owes a pass over both families.
## Table geometry pairs move together


`resize_tables`' Row branch keyed its keep on the row count alone. Two row levels share a row count at different hash logs once the source-length clamp shrinks the head table: level 10's rowLog 5 at srcLog 11 and level 5's rowLog 4 at srcLog 10 both give 128 rows. The kept-rows branch resized `tables` while `second` still named the old span, and the per-thread bulk pool carried the stale pair into a later Fast frame, whose heads-keep branch truncates but cannot grow. `parse_fast` then sliced `tables[..second]` past the buffer.

The crash input passes standalone because the poisoner and the victim are different inputs in one thread, so the artifact walker's fresh process per input cannot see the class. Pin it with an in-process call sequence through `encode_edges::pooled_row_geometry`. Any future kept branch that resizes one field of a pair must update its twin in the same branch.

- Guard the small-literal u64 copy on the read side with `anchor_idx + 8 <= win.len()`. Write-side overflow is harmless, while a read-side out-of-bounds read intrudes into illegal memory.
- Use `saturating_sub` for loop guards. Miss stepping can cross `block_end`, and unsigned subtraction wraps into a huge value.
- `insert_max` subtraction underflows on a first block below 5 bytes, where `win_base + win.len() - MIN_HASH` goes negative. Compute it lazily or use `saturating_sub`.
## Merged add width over the accumulator cap


`pack_seq` 把三个字段的 add bits 合并成一个 payload，宽度 `ln + mn + on`。ln 上界 16（LL code 35，base 65536），mn 上界 16（ML code 52，base 65539），on 上界 27（`encode_offset` 取 wire 值的 ilog2，window log 被 clamp 在 27，wire 值 dist+3 不超过 2^27+2）。ln=16 与 mn=16 无法共存：两个 base 之和 131075 已超过 MAX_BLOCK_SIZE 的 131072，装不进一个块。可行组合的 ln+mn 上界是 31（16+15），因此 add_nb 最高到 58。

`hot_push_raw` 的合同是 flush 后 bits < 8、任何 nb ≤ 56 都安全。nb=58 且 pending bits=7 时 `acc |= v << 7` 丢掉 payload 最高位，bits 变成 65；下一次 flush 的 `acc >>= 64` 在 debug 和 paranoid 下是 shift-overflow panic，在 release 下回绕成 `>> 0`，累加器从此失步，序列段的整个尾部损坏。绕过 56 上界的调用有两处：最后一个序列的首 push 完全没有宽度检查；split 分支的 guard 只检查了合并形态 `trans_nb + add_nb <= 56`，进入该分支后单独 push `add` 却没有再检查 add_nb 自身。

触发条件苛刻，需要 w27 窗口加一条 ll+ml 不小于 98307 的序列，所以全部门禁都是绿的：bench 语料的窗口不超过 25，on ≤ 25、add_nb ≤ 56。修复是 `push_add` 在 add_nb 超过 56 时按字段拆分 push，payload 布局 `la | ma << ln | oa << (ln + mn)`，三个宽度从 code 字节恢复（LL 与 ML 走 LL_META/ML_META，offset 的 code 就是自身宽度）。拆分 push 与单次 push 的位序完全一致，先低字段后高字段，所以 add_nb ≤ 56 的所有输入保持 byte-identical，merged 快路径不动。配套的墙有三层：`seq_codes.rs` 的 `const _` 网格证明所有 base_ll + base_ml ≤ MAX_BLOCK_SIZE 的 (lc, mc) 组合满足 ln + mn ≤ 31，`hot_push_raw` 入口 debug 断言 nb ≤ 56，`encode_offset` debug 断言 log ≤ 27。容量合同写在注释里不会约束调用方，要么在每个调用点检查，要么在入口断言。
## finish 的 last-block 补丁越过已交付字节


ST 流核心 finish 的 `BlockTail::Open(off)` 补丁分支只检查了 `off < self.output.len()`。`output` 只在完全排空时清空；一次失败的 drain 把 `out_read` 留在缓冲区中间，已交付前缀仍然留在 `output` 里。write 侧的文档合同明示失败后可以重试（输入可能已被接受，重试只补发未交付的输出），所以报错后调 finish 是合法用法。

可达路径是：底层 writer 接受了覆盖最后一个块头的字节后报错（WouldBlock 或 ENOSPC），用户调 finish，补丁打在已交付的 output 字节上，线上看不到这次修改；随后不再发空闭合块，校验和照常追加，finish 返回 Ok。wire 上是 last 标志缺失的无效帧，解码器在校验和字节处把块头读成 reserved 块。MT 核的输出不走这个原地补丁形态，不受影响。

修复是补丁分支的 guard 增加 `off >= self.out_read`，不满足时自然落到空闭合块分支，也就是 eagerly drained 消费者的既有形态。教训是：原地补丁只对仍然归编码器所有的字节有效，一旦存在 drain 游标，"位置在缓冲区内"必须收紧为"位置在未交付跨度内"。
## finish 的 last-block 补丁打在 split 首分片上


ST 流核心的 `emit_block` 在整块编码之前记录 `BlockTail::Open(self.output.len())`，语义是"这个块的头"的偏移。opt 行（level 不低于 17 且窗口不低于 2^17）的 post-parse splitter 会把一个 128 KiB 输入块写成多个 wire 块，每个分片各带 3 字节头，last_block 只落在最后一个分片上。"块的头"从此不再唯一，finish 的原地补丁把标志打进了第一个分片的头。第一个分片之后的字节（其余分片与 4 字节校验和）成为帧外垃圾，解码器以校验和不匹配拒绝该帧。

触发面是两个公开用法。raw::Encoder 文档示例的节奏是 write、finish、再投递，finish 时 output 非空，补丁分支命中。write::Encoder 的 sink 报一次瞬时错误（例如 WouldBlock）后，未交付尾部留在核心里，用户调 finish 同样命中。同样的输入，投递节奏决定帧是否可解。pledged 流的最后一个块在 write 路径已带 last 标志离开，两种节奏都正确。

修复让 `compress_fastest` 返回最后一个 wire 块头的偏移，split 路径经 `SplitOutcome::Emitted { last_header }` 透传，`emit_block` 在编码完成后记录该偏移。教训是：引入 post-hoc 分片后，指向"块头"的锚点必须写明是哪一个 wire 块的头，取 last-block 语义时只能是最后一个。
