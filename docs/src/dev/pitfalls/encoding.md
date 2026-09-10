# 踩坑记录 · 编码侧

## 匹配器

- **commit_space 把 pos 游标设到块尾 → matcher 全空转**（所有块退化纯 literals），
  而重建型单测照常通过——必须配 skip_matching 索引有效性断言与 repcode 发射断言。
- 后向扩展后 offset 必须用最终位置差 `start - cand`。
- **rep1_chain 返回后必须 `anchor_idx = ip_idx`**（三处发射路径都要），否则链覆盖
  区被下一序列再计一次字面量 → 解码 TargetTooSmall（从 fast 老代码移植时丢过这行）。
- chain 循环 repcode 向后扩展必须保留 ≥1 字面量（floor=anchor+1）：ll=0 时
  of_value 1 是 rep 交换语义（`do_offset_history` 的 ll==0 分支），周期数据必炸。
- chain 循环 pos==anchor 时不探测 repcode 会输自家 Fastest 8 倍——须镜像 fast 的
  probe=pos+1 语义。
- **索引域分裂（重大教训）**：chain 表 insert 用窗口索引、walk 用绝对位置，二者
  只在 `win_base == 0` 时一致；任何真实驱动（流式压实、bulk 逐块 adopt、MT job
  基址）推进 win_base 后，walk 全部跑在截断乱链上（有效 depth ~2-5）——"速度
  领先"是少烧 probe 的假象、ratio 亦偏差，且流式 4× 崩坏同源。
  **(a) 流式 vs bulk 输出不一致就是搜索状态分歧的烟雾报警（字节级相同断言此前
  只验过 Fast/Fastest）；(b) 索引域必须在 insert/lookup 两侧写同一条不变式注释。**
- u32 表项（`327bc99`）两坑：跨帧 stale 条目数值 > pos 且无 4GiB 周期可回退时
  `cand - 2^32` 必须 `checked_sub`（release 回绕成巨索引 OOB 读——首版 bench 带着
  bug 跑了一轮才复核发现）；`!u32::MAX as u64` 优先级陷阱（一元 `!` 先于 `as`，
  = 0，hi 恒零且静默）。**位运算助手函数必须配跨 2^32 边界的往返测试。**
- **u32 不清表的非确定性**：pooled 状态残留 + stride-3 网格只写 1/3 槽位 → 同输入
  同进程连跑尺寸跳变（json.Balanced 5514733/5515064 交替）——`327bc99` 起就有的
  潜在隐患，稠密填表此前掩盖了它。修：每 job 清 head 表（C 同款）；chain 链表
  可不清（归纳可证：链槽只在候选位置被读，候选只来自已清 head 或本 job 链接值）。
  替代方案全被否：frame-epoch 清表（调度相关）、job 静态绑定（丢负载均衡）、
  entry 打标签（热路径付费）。
- **raw 块回退是状态发散温床**：编码器任何跨块状态（rep / 复用熵表）都必须与
  "解码端实际收到的内容"对账——raw 回退要回滚 rep + 复用表，否则下一块
  Treeless/Repeat 引用解码端从未收到的表。
- `ip1_idx - anchor_idx` 下溢（emit 返回的新 anchor 可越过 ip1）：release 静默、
  debug panic 打断 MT worker → join 永等。修：改精确谓词，无减法。
- matcher scratch 驱动复现 MT job 场景：块必须 ≤128K（ml>131074 会 unreachable），
  adopt_window 只到 block_end（否则 extend_match 越块出巨 ml）。

## 熵编码 / 位流

- **Rust 运算符优先级**：`1u64 << 62 / total` 解析为 `1 << (62/total)`（`/` 高于
  `<<`），归一化 step 全错 → 比率 -28%。**移植 C 的 `f(a,b)` 形式函数调用时必须
  手动加括号。**
- **literals 5 位尺寸格式的 size_format 只占 1 bit**（zstd 规范：1-bit form 时
  bit3 是尺寸的一部分）；按 2 bit 写会错位整个后续块。Compressed 类型 sf=2/3 是
  14/18 位尺寸（4/5 字节头），与 Raw/RLE 布局不同。
- packed 码流抽取须逐段掩码：`(packed >> 8)` 带着 of 字段高位（可达 0xFFFF），
  强转 `usize` 前必须 `& 0xFF`——只有强转 `u8` 时截断才天然等价。
- `SeqWord.codes` 三字段是 **FSE code 不是原始值**（ml 是 ML code，ml-3 只在 ml<15
  成立）；解码诊断要用 META 基数或走重构路径。
- FSE -1 符号三坑：对拍输入的正概率和必须 = `table_size - count(-1)`（否则生成
  畸形测试输入误报）；SoA 构建时 -1 符号被 `prob <= 0` 过滤，其 start 状态必须
  在分配时显式写入；transitions 打包的 baseline（9 位）要求 baseline < 512，扩
  acc_log 须同步扩打包位宽。
- of_value 语义：非 rep 匹配 = offset+3，rep 匹配 ∈ {1,2,3}；诊断打印 offset 时
  307203 = 307200+3，别当独立常数。
- python 解析帧结构的坑（nbSeq 位宽、`+` 优先于 `|`、sort 混内核符号）见
  [工程方法论](workflow.md)。

## 内存安全

- **reserve 基准是 len 不是写入位置**：批量位写循环中 `pos` 常领先 `len`
  （set_len 延迟到收尾）；`reserve(固定值)` 一旦 capacity 足即 no-op → 后续 store
  越界破坏堆元数据（延迟爆 `realloc(): invalid next size`）。必须
  `reserve(pos + N - len)`。
- 哈希载荷改 u64 后，扫描尾部守卫（`block_end - pos < 8`）与插入上界（`len - 8`）
  必须同步收紧，防读 `set_len` 留下的未初始化尾部。
- 小 literal u64 拷贝：读端必须 `anchor_idx + 8 <= win.len()` 守卫（写端溢出无害，
  读端越界侵入非法内存）。
- 循环守卫用 `saturating_sub`：miss 步进可越过 block_end，无符号减法回绕巨值。
- insert_max 减法：首块 <5 字节时 `win_base + win.len() - MIN_HASH` 下溢 → 惰性
  计算或 saturating_sub。

## SPSC 校验和环

- **"单生产者"指单线程不是单帧**：嵌套/交叠帧（reentrant compress、同线程双帧
  测试）会交错进同一环；任何"建帧时缓存 next_seq 起点"的方案都会让两个活帧认领
  同一 seq 互相覆盖（内层 RESET 被踩 → worker 越界 panic → 主线程 FinishCell
  自旋死锁卡满核）。正确：**每 post 从共享 head 现读 seq 认领**（单线程内 post
  非重入，读取即认领），生产者本地只留 drain 阈值。
- **worker panic = 隐性死锁**：worker panic 后主线程所有 wait 变永久自旋且卡满核；
  排查先看 `--test-threads=1 --nocapture` 里的 worker panic 输出，再 pkill。
- 内存序：slot 载荷 Relaxed → kind Release → head Release；消费侧 head Acquire →
  kind Acquire → 载荷 Relaxed，链式 happens-before；环回绕由 wait_free_slot 守卫
  （与帧身份无关，天然支持交错）。

## 其他

- 流式 128KB 尾部空块怪癖：Stream 读满 tail 无法预知 EOF，必二次读取触发 0，多发
  一个 3 字节 raw last 块；slice 路径要输出逐位一致必须显式镜像 trailing_empty。
- **opt.rs（zstd_opt 移植）细节**：`#[cfg(feature="std")]` 的调试 eprintln 会随
  默认 feature 编进 release（曾静默刷屏 stderr）；`ZSTD_count` 返回指针差——续数
  要**替换**不能累加；rep history 由路径遍历每 series 更新一次（发射侧 per-seq
  更新是解码器等价语义，二者取一，重复更新必炸）；插树计数 cap 在 DP 窗（4096
  位）——否则对 parser 跳过的区域按 stale head 重计数（text 45→313 MiB/s 的教训）。
- 语料生成窄整数：`(i as u8 + 1)` 在 i=255 处 debug 加法溢出 panic（u8 先截断后
  +1）；要 usize 算完再 cast。
- gain 门的语义偏差：C 的 +7 是"是否用后续候选**替换**已持有匹配"的裕度，不是
  store 门；我们把门放 store 决策上（比 C 严）是刻意的（我们的表更密），但要知道
  代价是 text 中短距匹配误杀。
- **FSE 转移表打包位宽暗约束**：u32 entry 的 baseline 只留 9 位（表 ≤512 状态），
  生产 acc_log ≤9（huff0 权重表 6、序列表 9）从不越界；fuzz_exports `round_trip`
  的 max_log=22 构出 log10 表 → baseline≥512 静默截断 → 编码器写出超位宽 diff
  （fuzz 下 debug 断言炸，release 静默错位流）。修：布局加宽为 12+4+12
  （`optimal_table_log` 本就 clamp 5..=12），`build_table_from_counts` clamp 到 12 +
  `build_table_from_probabilities` debug_assert 双保险。**教训：位域打包的隐式上限
  必须在构造入口强制；"生产用不到"的参数范围迟早被 fuzz 或后续扩展踩中。**
- **ST 编码输出依赖进程内历史（matcher 状态池残留）**：thread_local 匹配器状态池
  跨帧残留（327bc99 u32 不清表设计），同输入在不同进程内历史下可产出**不同但均
  合法**的帧（Level::Fast、24B 输入实测：流式 37B raw 块 vs bulk 35B）。症状：
  单进程内复现"时过时不过"。约束：跨 build 字节对比必须新鲜进程（dump 工具即此
  用途）；对拍 oracle 用双侧解码合法性而非字节一致（encode_stream fuzz 即此）。
  MT job 的同类问题已由 a6cf8a6 清表修复（踩坑 20 的 ST 变体）。若将来要 libzstd
  式"同输入同输出"确定性：reset 清表（速度代价）或 frame-epoch 方案（见 WORK.md
  踩坑 20 论证）。
