# 现状总览

> 截至 `27b91cf`（2026-09-10）。分支目标（AGENTS.md）：功能与原版 zstd 相当、
> 性能超越原版；不上游，允许任意 unsafe / SIMD / 新指令集。

## 能力矩阵

### 压缩级别

| Level | ≈zstd | 匹配器策略 | 窗口 | 备注 |
|---|---|---|---|---|
| Uncompressed | 0 | raw 块 | — | |
| Fastest | 1 | fast（hash5 单探测表） | 768 KiB | |
| Fast | 3-5 | dfast（hash8+hash5 双表单探测） | 1 MiB | |
| Balanced | 6-9 | hash chain + lazy | 1 MiB | H20 / depth 8 |
| Best | 10-15 | optimal parser 低配 | 1 MiB | 16 compares / targetLength 32 |
| Opt | 16-17 | btopt（全量移植） | 1 MiB | |
| Ultra | 18-22 | btultra(+2)（全量移植） | 1 MiB | 2-pass 首块统计 |

`approximate_zstd` 把数字 1-22 映射到最近档；CLI 接受全部级别。
窗口固定是**有意取舍**：2-4 MiB 裸扩窗实测双输（见[已证伪方向](dev/negative.md)），
扩窗需与 LDM 一起做。

### 编解码路径

| 能力 | 状态 |
|---|---|
| slice/bulk 编解码 | ✅ 编码零拷贝 + thread_local 状态池；解码 flat 直写 |
| 流式编解码（read/write Encoder、Decoder） | ✅ 无 flush 时流式输出与 bulk 字节一致 |
| MT 编码 | ✅ bulk（overlap job）+ 流式（burst）；workers>1；no_std 报 Unsupported |
| MT 解码 | ✅ restart-point 分段；stage B 串行为瓶颈，暂无扩展性 |
| 帧校验和 | ✅ 编码可选（+sidecar 线程卸载）；解码树内 xxh64 自动验证（MT 路径不校验） |
| zstd-crate 兼容层 `zstdx::compat` | ✅（词典解码端到端可用） |
| 词典 | 解码 ✅；编码 ❌；`dict/` 训练半成品（有已知 bug） |
| LDM / superblock / 可调窗口 / C FFI | ❌ |

## 与 libzstd 的完成度（主观估计，供定向）

| 维度 | 估计 | 依据 |
|---|---|---|
| 解码功能/正确性 | ~90% | spec 合规、词典解码、corpus+fuzz；缺 MT 路径 checksum |
| 解码性能 | bulk 全面反超；流式 ~75-85% | 流式残余在 json/skewed，见[当前快照](dev/bench/snapshot.md) |
| 编码功能 | ~60% | 七档阶梯 + MT + 流式已立；缺词典编码、LDM、superblock、可调窗口 |
| 编码压缩率 | 全档对位 | Ultra json 7.52 vs zstd-19 7.49；Opt 反超 zstd-16；Best 反超 zstd-12 |
| 编码性能 | 分档互有胜负 | text/skewed/zeros 多档领先；json 低档落后 1.3-1.8×；Best/Opt/Ultra 速度落后 |
| API/生态 | ~45% | bulk + streaming + compat + CLI；缺 C FFI、语言绑定、标准 CLI 参数面 |

注意：`COMPARE.md` 的完成度评估停留在 `4ff2b7b` 时点，其"编码功能 ~40%"、
"json.Best ratio 差距（btopt 缺口）"、"MT ratio 崩塌"等结论已被其后的
optimal parser（`c726dfd`）、package-merge Huffman（`aa07308`）、Best 换核
（`b39a192`）、MT ratio 保持（`a37ebaa`）刷新。

## 代码结构（crates/zstdx/src）

| 模块 | 内容 |
|---|---|
| `bit_io/` | 位读写器（反向位读 BitReader、BitWriter） |
| `blocks/` | 块级解析 |
| `decoding/` | 解码器：frame/block/literals/sequence 解码、`sequence_execution.rs`（融合执行）、`flat_buffer.rs`（流式 flat 窗口）、`ringbuffer.rs`（字典路径）、`mt.rs`（分段并行） |
| `encoding/` | 编码器：`frame_compressor.rs`、`match_generator.rs`（fast/dfast/chain 匹配器）、`opt.rs`（最优解析）、`levels/fastest.rs`、`mt.rs`（bulk MT）、`async_checksum.rs`（sidecar 校验和）、`blocks/compressed.rs`（熵编码块）、`seq_codes.rs` |
| `fse/` `huff0/` | 熵编解码 |
| `xxh64.rs` | 树内校验和（编解码共享；运行时零外部依赖） |
| `bulk.rs` `stream/` | 一次性与流式 API（`encoder_core.rs` / `encoder_mt.rs`） |
| `compat/` | zstd-crate 兼容层 |
| `dict/` | 词典训练（feature = "dict_builder"，半成品） |
| `level.rs` `options.rs` | 级别枚举与编码/解码选项 |

## 验证纪律（每笔提交）

原子提交（单行 msg）+ fmt + Changelog 条目 + debug/release/no-default 测试全绿 +
全语料与 libzstd（CLI）互认往返 + corruption smoke（随机损坏 0 panic）+
dump 对拍（确定性输出作免费回归探针）+ 性能 A/B 用 git stash 背靠背同机复跑。
细则与反例见[工程与基准方法论](dev/pitfalls/workflow.md)。
