# 基准测试方法论

> 测试环境：AMD Zen4 32C（AVX-512/BMI2/VAES），频率漂移明显（lscpu scaling ~69-73%），
> 噪声 ±10% 且有慢漂移相位。Rust release + codegen-units=1；zstd 参考侧同基线指令集。

## 语料

`bench/corpus`，32MiB × 5 形状：json（半结构化记录）/ text（源码树平铺）/
skewed（16 字母表）/ random（不可压）/ zeros。解码用 zstd CLI 预压的 zst1/3/9。

陷阱：

- **语料不可比**：`gen_corpus.sh` 的 text.raw 平铺仓库 src 树——src 一变语料就变。
  历史上两次"text 形状相变"（ratio 299.6 与 239 的跨 tile 匹配解锁）都依赖
  重复周期恰落在窗口区间；src 膨胀超过 768K 时该形状会再次塌陷。
  考虑固定一个 tar 快照。
- 语料生成的窄整数算术：`(i as u8 + 1)` 在 i=255 处 debug 溢出 panic（u8 先截断后
  +1）；要 usize 算完再 cast。
- zeros 是免费噪声标定格（代码路径零重叠仍漂 ±2.6%）。

## 工具（crates/zstdx-bench）

独立 crate（`cargo run --release -p zstdx-bench -- <子命令>`），bench/dev 工具不再
寄生在 zstdx 的 examples 里（也消除 no-default-features 检查重编 example 覆盖
release 构建的坑）。重矩阵不常跑，日常迭代用筛选跑局部。

| 子命令 | 用途 |
|---|---|
| `matrix` | 广域矩阵：`--mode dec-st/dec-mt/enc-st/enc-mt/enc-stream/all` 五段，交错 A/B + roundtrip 门；`--shape/--level/--workers/--mt-workers` 筛格子，`--budget-ms` 控每侧预算 |
| `small` | 1KiB-1MiB 小负载；`--size`/`--impl` 钉死单尺寸单实现（profiler 定位用）|
| `files` | 任意 .zst 文件解码计时（预算制；自动按任意 `zst*` 后缀找 `.raw`/裸 stem 参照校验）|
| `prof` | 单侧 profiling 循环：`prof dec <f> [n]` / `prof enc <lvl> <n> <f...>` / `prof enc-stream <lvl> <n> <f>`；`RUZ_CKSUM` env 切校验和路径 |
| `dump` | 语料字节快照，确定性回归探针：两 build 各 dump 一目录 `diff -r`；`--all-levels` 全 6 档（tag l1/l3/l6/l12/l16/l19）|
| `corrupt` | 随机损坏冒烟（0 panic），覆盖流式 + flat 路径 |
| `mtcheck` | MT 解码 vs zstd CLI 对拍（`--workers` 选档）|
| `seqstats` | 序列统计与熵下界 |
| `prefill` | prefill_window micro（`--shape/--level` 选档）|

公共 harness（crate 内 `src/common.rs`）：逐轮交错、warmup、时间预算（`--budget-ms`，
默认 500ms/side；等价旧 BENCH_BUDGET_MS env，env 仍生效）、median/mad 统计。

已删的一次性工具（均被覆盖）：bench_compare / bench_encode（matrix `dec-st`/`enc-st`
+ 筛选）、bench_corpus（files）、ab_fast / ab_mt（matrix + 筛选）、compression_ratio
（small）、stream_cmp（fuzz decode 的小块流式读）、criterion decode_all（files）。

## 判据纪律

1. **单次结论必须二次复跑**；跨时段只认 git stash 背靠背同机 A/B。
2. **确定性输出 sizes 是唯一免费可信 A/B 信号**（不该改输出的改动）；速度需多轮
   中位或进程内开关，单轮 ±10% 内的"收益"不可信。
3. bench 全量跑完 decode 段后 encode 段绝对值整体偏低（zstd 侧也慢 18%）——
   横向对比以专用 A/B 工具为准，bench 只看稳定 ±mad 的格子。
4. micro 改动后期以 `perf stat` **指令数**为准（墙钟 ±5-25% 摆动常为布局/调度
   噪声）；端到端改动用交错墙钟。
5. 小负载判据一律单形状单尺寸进程（分配器交叉污染可致 3 倍失真）。
6. 10GB/s 级负载（text.zst3/z9）墙钟不可信：看 cycles 或只信 <9GB/s 格子。

## 口径要点

- **xslow = 我方时间 ÷ zstd 时间**，>1 = 我们慢；速度一律 MiB/s of raw。
- zstd 的 bulk(slice) 解码 API 是慢速包装（逐块重入），其真实速度在 streaming 路径
  ——**流式列才是真实差距**，bulk 列只作 API 口径参考。
- 编码侧双侧关帧校验和（我们 xxh64 开销单独测：json.Fast on/off = 1.036）；
  sidecar 卸载后的终态数字含校验和，对比时注意口径。
- MT 编码两侧均"每次调用冷线程池"（zstd 无 per-call 对等 API；warm-pool 单列参考）。
- MT 解码 libzstd 无公开 API，是我们独有维度（solo 扩展性表）。

## 矩阵覆盖缺口（后续可补）

字典编解码、pledge 已知尺寸的流式编码、>32MB 输入的 MT 扩展性、zstd CLI 多文件、
我们 MT 解码对自身 MT 编码输出的表现（restart 密度可控，理论上限更高）、
乱链修复 + opt parser + ratio 保持之后的**全矩阵重测**。
