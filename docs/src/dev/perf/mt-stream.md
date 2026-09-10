# 性能优化 · 多线程与流式

## MT 编码（bulk slice 路径）`fd931a1`

**job 模型**：单帧 overlap job（stage-2；frame-per-job 被否——多帧输出对解码侧是
负担）。输入切 job（共享公式 `job_size_for`：下限 1MiB、随 workers 均分、上限
1GiB——libzstd zstdmt 同款 cap）；`std::thread::scope` + 原子计数取任务 + condvar
汇合；worker state 走 thread_local 池跨调用复用；主线程写帧头（FCS=总长）+ 顺序拼
job 块流 + 校验和并行吸收；worker panic 由调用线程在拼装排空后恢复。

**job 独立性三条件**（每 job 起始处，zstdmt 同源核实）：

1. 熵表复位（last_huff/三个 previous = None → 块头显式表达，合法）；
2. repcode 哨兵 `rep=[u32::MAX;3]`（probe 必然越界失败，3 个字面 offset 序列后与
   解码器 rep 状态自然收敛；热循环加 `rep_pending` 门控后 ST 输出字节不变）；
3. overlap 前缀 prefill（见下）。

**ratio 保持**（曾为最大缺陷：text 周期语料 mt16 ratio 328.8→19.7，zstd-mt 189）：

- overlap=window 且条带**真正 prefill 进表**——原先条带只借作窗口但插入路径只插
  job 内位置，跨 job 匹配数学上不可能。`a37ebaa`
- fast/dfast/chain 条带填表走 **stride-3 网格**（`4bba04a`；chain 偏离 libzstd 的
  稠密字典填表是有意的：我们的条带=全窗，稠密填表 ≈ 半个 job 的扫描时长）；chain
  store 加 gain 门防条带洪泛出 5 字节垃圾匹配。
- **job 起点周期种子**：prefill 时对条带尾做后向扫描找最近长重复距离 O，job 开头
  直探 `pos-O`（3 次后 rep[0]=O 由 repcode 链接管；8192 探针预算退休死种子）。
  周期语料三档 mt/st 2.7-2.8→1.00。`a37ebaa`
- **确定性**：每 job 清 head 表（pooled 状态残留使输出依赖 worker 调度，±0.06%
  跳变；u32 不清表设计埋的雷，稠密填表曾掩盖）；chain 链表可不清（归纳可证：
  候选只来自已清 head 或本 job 链接值）→ 清表减半；≥4MiB 清表用 NT store。
  `a6cf8a6`
- **种子扫描 AVX-512**：occupancy 位图（64B 块 × 8 交叠 load 组装每位置一 bit，
  自顶向下取位 = 标量最近优先的精确等价，输出不变）；random 类 0.45→0.06-0.13
  ms/job（4-7×）。`e1c6d67`

战果：64MiB 文本 2/4/8 workers = 2.05×/3.95×/7.55×（比率损失 <0.1%）；32MiB
矩阵 mt8/16 速度 2-4.5× 领先 zstd-mt，ratio 修复后 mt16 各格回到 ST ±0.5%。

## MT 解码（独占维度：libzstd 无 MT 解码）`3219947`

- **restart-point 分段**：预扫块头，在熵状态自描述处（literals 非 Treeless、无
  Repeat FSE 流）切分段；job 型编码器（zstd `-T` 与我们 MT 编码器）在每个 job 边界
  恰好产生这些点；多帧输入 = 干净起点单元的特例，统一 unit 派发器。
- stage A（worker 池）并行熵解码产打包序列流 + 精确输出尺寸；stage B（调用线程，
  按输入序）执行进输出、携带 rep 历史——repcode 是唯一跨序列状态且不触 stage A。
- 字典帧、malformed、单 restart、单核 → 串行回退（错误报告归串行路径）。
- **现状**：64MiB 2.06×(4T)/2.10×(8T) 后再无扩展；stage B 串行占 45-80%
  （skewed.zst9 匹配拷贝占 80%），stage A 并行只添乱。stage B 并行化
  （reachback 分析 + rep 历史前缀扫描）是最大待办——做好即从 0.66-1.0× 负资产
  跳到独占领先。

## 流式编解码（ST）

- 流式编码器（write::Encoder / read::Encoder）共享增量核心，与 bulk 同
  matcher/块编码器积木；无中间 flush 时输出与 `encoding::compress` 字节一致
  （跨 write 分块断言）。`51ec2fc`
- 流式解码 flat outBuff 模型与 `StreamingDecoder` read 粒度优化见解码侧页。
- read/write Decoder 对多帧与 skippable 帧透明（`single_frame()` 恢复单帧语义）；
  write 侧按块头+体+trailer 完整暂存才交 FrameDecoder（饥饿读会毒化状态）。

## 流式编码 MT（burst 模型）`44e11e5` `27b91cf`

- `FrameEncoderCore` 改 enum `Single(Box<FrameEncoderCoreSt>) | Mt(MtEncoderCore)`
  （std 门控），read/write 路径零改动；no_std workers>1 保持 Unsupported；路由
  （单核 / raw 级 / Uncompressed → Single）与 bulk 一致。
- 单连续缓冲 `[上一 burst 保留的 window 条带][未编码数据]`（无 memcpy 滑窗）+
  **绝对对齐 job 网格**（job k = job_start + k·job_size）→ 无 flush 时输出与
  write 分块方式无关（确定性；flush 是文档化例外，rebase 网格）。
- 攒够 max(workers,2) 个完整 job 触发 burst：scope 并行 + 主线程并行吸收校验和 +
  顺序拼装 + poison 传播；单 job 收尾内联不 spawn。
- **pledged**：共享 bulk job-size 公式切 job + hold-back 最后一个 job 给 finish
  标 last → 与 bulk MT 字节一致（32MiB 实测 sizes 全等）。代价：该 job 在 finish
  内联单线程跑——fast 档慢 0.55-0.78×，best 档反而快 1.24-1.28×（大 job 摊薄
  per-job 固定成本）。hold-back 必须带 `pos <= n` 守卫（超写 pledge 时 pending
  无界增长）。
- **encoder 持有跨 burst worker-state 池**（`Mutex<Vec<Box<CompressState>>>`）：
  thread::scope 每 burst 新线程，thread_local 池永远 miss（Balanced 8MiB 表 +
  首触缺页），deep 档因此 +22-30%。panic 的 state 可能 mid-compress 垃圾——丢弃
  不归还。
- 战果：json.fast 3.5× vs zstd stream-mt8；json 类对自身流式 ST 3.7-3.8×；
  完整数据见[矩阵页](../bench/matrix.md)。
- **残余结构性差距**（均已验证非 bug，见待办）：read::Encoder 泵 16KiB 双拷贝
  （直读未初始化缓冲违背 Read trait 契约，已评估放弃）；每 burst spawn 8 线程
  （~0.5-2ms/burst，持久线程池属设计外重构）；unpledged 1MiB job 的 strip 全量
  prefill（每字节成本 1× vs bulk 0.5×；加大 job 伤小流并行度与首输出延迟，
  维持设计值）。
