# 待办清单

> 已对照 Changelog（截至 `27b91cf`，2026-09-10）核销：过程文档里列过但已落地的项
> 不再出现（如 json.Best ratio→已由 opt parser 收口、MT ratio 保持→`a37ebaa`、
> 流式 MT→`27b91cf`、表项 u32→`327bc99`）。证伪过的子方向见
> [已证伪方向](negative.md)，勿因本清单复活。

## P0 · 结构性

1. **MT 解码 stage B 并行化**（reachback 分析 + rep 历史前缀扫描）
   - 独占超越维度：libzstd 无 MT 解码；做好即从 0.66-1.0× 负资产跳到独占领先。
   - 现状：mt2-16 无扩展性（stage B 串行占 45-80%；skewed.zst9 匹配拷贝占 80%）。
   - 思路：按段预测 reachback 深度定切分点；rep 前缀用 stage A 的序列流提前重放。
2. **MT 解码路径 checksum 校验**：现明示不校验；解码端 ST 已自动验证（`81d2119`），
   MT 路径补齐并复核 mismatch 报错行为。
3. **广域矩阵重测**：编码侧数字停在乱链修复前后（矩阵 §3/§4/§5），其后落地了
   opt parser（Best 换核）、u32 表项、MT ratio 保持、stride-3 prefill、流式 MT；
   §4 MT 表明确作废。zstdx-bench `matrix` 工具就绪，跑一轮更新
   [矩阵页](../bench/matrix.md)与[快照](snapshot.md)。

## P1 · 编码速度

4. **json.Fastest ~1.78×**（最大单项速度差距）：扫描循环与 zstd -1 的差距（zstd
   双位置 ip0..ip3 软件流水 + prefetch + cmov 判断；我们两位置）；
   encode_sequences 后向 FSE 循环 ~12.6%（每序列 87 inst，对照
   `ZSTD_encodeSequences` 找差距）；字面量 huff0 路径。注意 select 化/cmov/上下文
   结构已做（`29bcb02`/`c528c57`），剩余不在 matcher 微观手法，先 profile 定位。
5. **json.Fast 1.35× 残余**：每序列成本候选——encode_sequences 12.6%、
   choose_tables_fast 直方图三通道 SIMD、const-generic log 消 `shr %cl` 可变移位。
6. **Balanced 速度**（对 zstd-9 的 json ~1.2×；skewed 已由 prefill 反超）：转向
   **每探针/每位置成本**——走链 beat-check/unpack 序列与 emit 路径指令量
   （enc_prof + perf 定位）。miss 段策略类方向（步进/插入调度）已两次证伪关闭。
7. **random 低档 1.18-1.46×**：raw 块逐块开销（zstd raw 块近乎 memcpy）；
   嫌疑：每块固定成本（判定/stage/emit）、dfast miss 步进块间清零、chain 走满
   8 深垃圾链。待 profile 再立项。
8. **Best/Opt/Ultra 速度**：ratio 已反超/对位，速度是剩余维度——text Opt 313 vs
   libzstd ~440；Best 12-19 MiB/s 的每块 DP 成本；流式 MT 的 best 档 per-job
   固定成本（text.best 流式 0.8× 自身 ST）。
9. **小中负载（4K-1M）**：json/text 中尺寸 ~2×（扫描+emit+建表每字节成本，非
   固定开销）。候选：Huffman `build_from_weights` 排序 → 12 桶计数排序（权重 ≤11，
   桶间降序桶内 symbol 升序 = 现语义严格等价；text-4K 建表占 ~18%）。

## P2 · ratio 与功能

10. **词典编码闭环**：编码器支持用词典（frame header dictionary_id 恒 None）；
    修 `dict/` 训练 bug（epoch 缓冲硬编码 `vec![0;100]`、打分遍历
    `collection_sample` 而非当前 epoch）或干脆移植 C fastCover。
11. **LDM**：gear hash 长距匹配；与裸扩窗解耦（2-4MiB 裸扩已证双输，但 LDM 未测）；
    可复用现有 matcher 融合（C 的 optLdm 长距候选注入可参考）。
12. **流式 MT 残余**：每 burst `thread::scope` spawn 8 线程（~0.5-2ms/burst；
    持久线程池属设计外重构）；unpledged 1MiB job 的 strip 全量 prefill 成本
    （设计取舍，pledge 用户已可获 bulk 同构效率）；zeros.Balanced 0.8× 的每 job
    NT 清表固定成本（RLE 语料，低优先）。
13. **远期**：superblock（小数据 ratio）、preSplit 内容自适应分块、AVX2/SSE2 中间
    SIMD 档（nested `#[target_feature]` + `#[inline]` 非 always 的 stable 写法已
    验证；风险是 N 份实例化的 codegen 干扰）、C FFI + zstd CLI 参数面对齐。

## 搁置（有明确结论，勿盲目重启）

- **流式解码 decode_step 重构**（json/skewed 残余 1.05-1.3×）：寄存器墙已证
  （活跃值 ~20 > 15 GPR；结构改造三连证伪），除非先减活跃值否则无空间；下一量级
  需 libzstd 式 8 深序列环形缓冲流水类算法级变化，先评估收益/风险比。
- **read::Encoder 泵双拷贝**：直读 spare capacity 需对任意 Read impl 传未初始化
  缓冲，trait 契约不允许，已评估放弃。
- **BMI2 分发的 Intel 实测**：Zen4 中性零成本保留；有 Intel 机器时可补数据。
- **madvise(MADV_HUGEPAGE)**：THP=always 机器冗余；换 THP=madvise 机器可重试
  （extern "C" 声明零新依赖路线已验证）。
