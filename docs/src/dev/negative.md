# 已证伪方向（勿重试清单）

> 每条都有 A/B 数据支撑。若前提变化（表结构、窗口、语料、CPU），允许重新评估，
> 但必须先拿到新的分解证据再动手；对拍纪律见[基准方法论](../bench/methodology.md)。

## 解码端

| 方向 | 结果 | 结论 |
|---|---|---|
| 融合循环拆批处理（decode-batch→exec-batch 双循环，3 变体） | -15~-21% | 中间 Vec 往返非瓶颈；批缓冲破坏 LLVM 寄存器驻留并损失融合乱序重叠 |
| 两阶段批处理流水线（batch 16，代码生成完全达标） | cycles +9-15%、miss +14-30% | 融合循环里 decode/exec 交错的分支流对 TAGE/BTB 承重，拆成两股纯流毁历史相关性——**融合结构局部最优** |
| repcode 寄存器化 + 全无分支解析（融合循环内） | json -2%/miss -22% 但 skewed +8-11% | 寄存器预算饱和：+3 活跃值必挤出更热值；分支消灭的收益 < 溢出代价 |
| 纯 branchless repcode（cmov 链 + `%3` 折叠伪槽） | json 回退 5-6% | repcode 分支预测意外地好；**去分支 ≠ 提速**；最终版=解析侧 1 cmov + 更新侧单分支 |
| 3 FSE state 打包单 u32/u64 | 回退 | pack/unpack 落在串行 FSE 链上，比省下的 spill 更贵 |
| 4streams X1 循环 BMI2 化 | 指令 -12.7% 但 json/skewed -2~3% | 前端/代码布局效应吃掉收益；该循环 1.42 cyc/符号 ≈ 理论极限 |
| 拷贝循环 32B 配对重构 + AVX2 copy32 | miss 大降（json -22%）但墙钟反跌 | **miss 减半 ≠ 时间收益**；json 类负载非 miss 主导 |
| copy_inline 手写内联拷贝（大拷贝语料） | memmove 33%→1.15% 但端到端零收益 | 大拷贝是带宽限制的必要功；与小拷贝语料上 wildcopy 成功不冲突 |
| literals 直写 flat out | 前提不成立 | 输出中 literals 与 match 交错，连续排布无法 no-op；直写只省中转不减写入次数 |
| X2/X1 huffman 循环再优化 | 收口 | ~0.75 cyc/B 与 libzstd 手写 asm 同量级；SIMD bit-serial huffman 无常规路径 |
| FSE 建表再优化 | 收口 | 4.9%→1.9% 后单项均 <4% |
| xxhash AVX-512 vpmullq 向量核 | 反而更差 | vpmullq 延迟（≈4cyc）破坏串行链；twox 4 链已贴标量机器极限；**正解 = 8 链标量交错** |
| madvise(MADV_HUGEPAGE) | 噪声内 | THP=always 机器冗余（已回滚；THP=madvise 机器可重试） |
| wrapped 分支 select 化 | miss 不动、json -2.9% | 该分支（74% taken、随 pos 漂移）本身预测良好；**select 化前先 stub 归因** |
| ll>0 无条件化（ll==0 也 copy16，垃圾被覆写） | skewed miss -62% 但 text +9.3% | 删分支本身的重排效应伤关键文件（TAGE/BTB 历史） |
| 快路径内联进融合循环 | json -1.2% | 热循环布局被扰；**新增代码一律进已有 out-of-line callee** |
| 实例化瘦身（缩 HEADROOM=false 体积） | 无作用机制 | 流式热路径只用 HEADROOM=true 实例化；错误路径出线化有 homing 风险 |
| AVX2 const 线程化 dispatch（8 实例化） | 可行但回退 | nested target_feature 写法可行；codegen 风险大（中间档待办里保留） |

## 编码端 · 匹配器

| 方向 | 结果 | 结论 |
|---|---|---|
| hash4 复活（@2^13 短表副表 / 16KB 距离上限 / @2^14 单表 / rep0 兜底，共 4 次） | 全败（json 4.18-4.93 均劣化） | 高频 4 字节样板的表污染不可逆；**hash5 是定论** |
| hash4 近距副表（ml≥6 门槛，治好污染） | 全线速度 -7~-14%、比率持平 | ml≥6 确实治好污染病，但收益抵不过双表成本 |
| 3-rep 探测（rep1/rep2 一并探） | 命中率 7.6%→26% 但 json/text/skewed 比率全变差 | 弱 rep 匹配偷走同位置更长的 hash 匹配；libzstd-fast 只探 rep0 是对的；rep2 尾探亦轻微劣化 |
| rep0 阈值 4→5 | 中性 | ml=4 rep match 太少 |
| HASH_LOG 17 | json +0.6% 但速度 -8%、skewed 比率劣化 | |
| 分级 MIN_MATCH（大偏移要求 ml≥5） | 零效果 | 远距 ml=4 本就稀少 |
| Fastest 窗口 <448K 或 2MB | 256K/384K 灾难（text 比率 301→7.88）；2MB 劣化 | tile 周期被切断是灾难；512K 可用但 0x70000 更 cache 友好（后续 768K 解锁跨 tile） |
| dfast 窗口 2MB | json 比率 -1.1% 且慢 4% | 远距候选抢占近距更优匹配的单探测槽 |
| chain 窗口 4MB（W4M） | 全形状双输、skewed 14 MiB/s | 只找到更远不找到更长，offset 码位损失 > 长度收益；扩窗须配 LDM |
| C 的 anchor 距离 miss 公式移植到 fast/chain | 全面回退（skewed 扫描述慢 ~80×） | 我们 stride-1 稠密插入 + newest-wins 单候选表，逐字节探测用垃圾覆盖远距好表项；C 公式前提（稀表 + hLog 大）不成立。仅 dfast 成立（`0b46f21`） |
| lazySkipping 完整移植（chain） | 11 轮交错全平（skewed +3.1% text -0.7%） | C 机制两根杠杆（密插→稀插、慢步进→快步进）我们设计里已花掉；**miss 段策略类方向关闭** |
| 纯双锚点插表（libzstd fast 默认） | json 5.71-5.90、text 275-290 | 比率损失严重；必须保留 ≤16 稠密插入 |
| 匹配内插入步进 1→2 | json 比率 6.02→5.97 | 换 json +2% 速度不值 |
| 无斜率全密度探测 / 斜率放缓 mc>>3 | json/skewed/random 全崩 | 插表量↑→2^15 表老化↑→远匹配死；miss 动态斜率是刚需 |
| hLog16（512KB 表，2^15 时代） | 速度 -8-14%、比率不涨 | 超 L2 局部性 |
| 全量 u64 哈希（不截 40-bit） | json 双赢但 text 比率 -7% | 40-bit 掩码保数学等价，全局比率优先 |
| 表项 u64→u32 @Fastest 2^15（2-bit epoch 方案） | 输出一致但指令 +0.09% | 表驻 L2 时宽度不是杠杆；对比：Balanced 16MiB 工作集时 u32 化是大赢（`327bc99`） |
| emit 跳过 scan pair 已插位置（幂等插入） | 输出 DIFFERS / 修后指令 +2.6% | 并非恒幂等（后向扩展时哈希碰撞覆盖，冗余重插有恢复槽值作用）；代价也超收益 |
| choose_table 朴素 Predefined/Repeat 阈值 | 可忽略 | 后由成本对比法（selectEncodingType 移植）+ repeat 模式完成；勿用朴素阈值 |

## 编码端 · 熵 / 校验和 / 其他

| 方向 | 结果 | 结论 |
|---|---|---|
| 块级预门（整块采样熵 ≥ 阈值直接 raw、跳过扫描） | 不可行 | **字节直方图无法区分纯随机与含远距重复的随机**；libzstd 也只对 literals 做 suspectUncompressible |
| 试探式 uniform 扫描（失配回滚重扫） | text -8% | 47% 字节是块首长游程，回滚重哈希翻倍；**正确语义是吸收式+续接** |
| encode_sequences 转移行预取（prefetchT0） | 指令 +3.5%、cycles 无改善 | ILP 已覆盖延迟，预取只添乱 |
| write_bits_64_cold 残余字节转 store | 墙钟剧晃、指令不变 | <0.5% 热点纯属破坏指令排布的调度噪声 |
| 直方图计数器 u64→u32 | 持平 | x86 `inc qword`/`inc dword` 吞吐相同 |
| Uniform-4 bulk 循环免检化 | 指令 -19% 但墙钟 -7% | 分支预测免费；微架构改动劣化指令寻址与流水调度 |
| ≤16 全密度插入循环 AVX-512 向量化（vpmullq） | 哈希微基快 2.2×、完整循环无收益 | 瓶颈是 256KB 表随机 store 吞吐（store 地板）；**只优化哈希计算的方案全部无效** |
| PGO（全语料训练） | 零收益 | 不值构建链复杂度 |
| 熵预检整数化 | 指令持平 | 92% 时间在直方图 incq，f64 熵计算近 0% |

## MT / 流式

| 方向 | 结果 | 结论 |
|---|---|---|
| frame-per-job MT 编码 | 设计期否决 | 多帧输出对解码侧是负担（单帧消费者不友好）；stage-2 单帧 overlap job 正解 |
| 仅放大 overlap、不 prefill 条带 | 无效 | 插入路径只插 job 内位置，条带永不被索引；放大 overlap 数学上无效 |
| 固定网格 prefill 找周期重复 | 数学无解 | 聚簇 5-gram 孪生槽位同值递归掩埋 / 相位对齐陷阱；dfast/chain 同受掩埋；**必须 job 起点种子** |
| 巨 pledge 无 job_size 上限 | 内存爆 | MAX_JOB_SIZE=1GiB（zstdmt 同款） |
| thread_local 池跨 thread::scope burst | 永远 miss | scope 线程每次新鲜；池须由 encoder 持有跨 burst 传递 |
| warm micro-bench 定量外推 MT memset | 严重低估 | 8MiB 驻 L3 ~70µs vs 真实 8 worker × 8MiB 互踩；改 NT store 才恢复 |
| 直读 Read 的 spare capacity（除 read 泵双拷贝） | 放弃 | 对任意 Read impl 传未初始化缓冲违背 trait 契约 |

## 方法论层

- 为减 branch-miss 盲目 branchless 化：先 stub 归因；miss 减半 ≠ 时间收益。
- 追求数出字节与 libzstd 完全一致：格式兼容即可；字节等价只作回归测试工具。
- 融合循环的寄存器预算已饱和：任何增加活跃值的"优化"先默认失败。
- 任何新增弱探测源（hash4 副表 / 3-rep / rep2 尾探）必须先 A/B 三语料比率——
  "弱候选偷位置"教训多次验证。
