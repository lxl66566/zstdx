# 已完成 · 功能与基础设施

> 性能优化的完整清单已按主题并入[性能优化](../perf/decoding.md)各页（每项带
> commit 与实测效果）；逐条变更记录以 `Changelog.md` 为准。本页记录功能、API、
> 正确性里程碑与基础设施。

## API 面

| 项 | commit |
|---|---|
| 根级 `Level` 枚举替代 CompressionLevel（删除从未实现的 Default/Better/Best 死变体） | `55daf71` |
| 高层一次性 API：`zstdx::{compress,decompress}`、`bulk::*`、`Error/Result` 伞、Encoder/DecoderOptions | `d35902b` |
| 流式编码器（write/read Encoder，`auto_finish`/`flush`/pledged/checksum/workers） | `51ec2fc` |
| 流式解码器（read/write Decoder，多帧 + skippable 透明）+ `encode_all/decode_all/copy_encode/copy_decode` | `b21d937` |
| zstd-crate 兼容层 `zstdx::compat`（bulk/stream 全套；词典解码端到端验证） | `4b60239` |
| CLI：clap 重构；全部数字级别可接受 | `c726dfd` 期 |
| 错误 thiserror 派生（首个外部依赖，compile-time only，no_std 兼容） | `72f3c04` |
| 解码器校验和树内化（编解码共用 xxh64；twox 降 dev-dep；运行时零外部依赖） | `81d2119` |
| dict_builder 模块改名 `zstdx::dict`（对齐 zstd crate 命名） | `7ad9e7a` |

## 级别阶梯与编码功能

| 项 | commit |
|---|---|
| Fast/Balanced/Best 三档落地（hash-chain matcher + clevels 对位参数） | `f8cc66d` |
| Fast 换 dfast 匹配器 | `19077f3` |
| Best 换 optimal parser 低配（16 compares / targetLength 32） | `b39a192` |
| Opt/Ultra：btopt/btultra 最优解析全量移植 | `c726dfd` |
| boundary package-merge 最优限长 Huffman | `aa07308` |
| 序列 FSE 表 repeat 模式（mode 3） | `7769bf8` |
| 帧校验和（hash feature 默认开）+ pledged_size + workers 选项 | 早期 |
| MT 编码 ratio 保持（overlap prefill + gain 门 + 周期种子） | `a37ebaa` |
| 流式编码 MT（burst 模型，workers>1） | `44e11e5` + `27b91cf` |

## 多线程

| 项 | commit |
|---|---|
| bulk MT 编码（overlap job、2/4/8 workers 2.05×/3.95×/7.55×） | `fd931a1` |
| 分段并行解码（restart-point 切分、stage A/B） | `3219947` |
| 校验和 sidecar 卸载 + worker 有界自旋停泊 | `df33295` `70b5fd4` |

## 正确性里程碑

| bug | commit |
|---|---|
| 解码端帧校验和自动验证（此前靠调用方比对 getter） | `81d2119` |
| 退化单符号 FSE 分布 panic + write_table 尾部零概率越界 | `b650cad` |
| raw 块回退不回滚 rep + 复用熵表（后续块引用解码端从未收到的表） | `61d63a9` |
| overlap_copy8 offset 5-7 的 usize 下溢（debug 挂 12 测试，release 侥幸正确） | `590177e` |
| MT 解码 literals 计数校验 bug（11 语料 7 个 MT 失败而仓库测试全绿） | `3964a62` |
| decode_to_vec_mt 串行回退对空 Vec 报错/死循环 | `21fae62` |
| chain 表 insert（窗口索引）/walk（绝对位置）索引域分裂（乱链） | `36203c1` |
| MT 输出非确定性（pooled 表残留；每 job 清 head 表） | `a6cf8a6` |
| dfast backfill 插入谓词（step<4 代理漏插长匹配覆盖区） | `06b67dc` |
| opt.rs 调试打印随默认 feature 进 release | `8233b2f` |

## 基础设施

- **交错 A/B harness**（`examples/common/mod.rs`）：逐轮交替、warmup、时间预算
  （BENCH_BUDGET_MS）、median/mad 统计。`dc82c29`
- **bench 工具族**：bench_compare（slice/stream 对 + 四级对位）、bench_small
  （IMPL/SIZE 钉死）、bench_encode、bench_matrix（dec/enc × bulk/stream/MT 全矩阵
  + roundtrip 门 + checksum 开销行 + 双侧 worker 扩展性）。`c922b34` `b1dd010`
- **确定性回归探针**：dump_all_levels 字节级快照对比（"不该改输出的改动"的免费
  A/B 信号）+ corruption_smoke（随机损坏 0 panic，覆盖 flat 路径）。`318d8f7`
- corpus 生成器入库跟踪。
- profiling 临时工具（未跟踪）：enc_prof / dec_prof / ab_fast / ab_mt / ab_prefill /
  dump_all_levels / stream_cmp / mtcheck。
