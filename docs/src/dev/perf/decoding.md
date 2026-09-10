# 性能优化 · 解码侧

> 全部已落地，commit 可查；证伪过的方向见[已证伪方向](../negative.md)，
> 未决残余见[待办清单](../todo.md)。

## 输出路径演进

1. **ring buffer**：手写 ringbuffer（"头部丢弃"与"内部拷贝到尾部"各需一半，
   VecDeque/Vec 各只能高效做一个），含 modulo-free wrap 与四布局手写
   extend_from_within。现仅字典帧使用。
2. **flat 直写（slice）**：无字典 slice 解码把 Raw/RLE/Compressed 全部直写调用方
   目标缓冲，绕过 ring 与 drain 拷贝；块级预算检查 + `TargetTooSmall` 错误变体。
   slice 全面 +13-35%。`77b03b6`
3. **流式 outBuff 模型**：libzstd outBuff 模型，稳态容量 window+2·block+64，
   两代虚拟映射 wrap（`FlatView{origin, prev_origin}` 维护全局单调虚拟地址），
   零滑动拷贝、flush 不留 window。大窗语料（8MB 窗）+85-125%，random/zeros
   反超 zstd-strm。`8426ba8`

## 融合序列循环（当前核心）

- **解码-执行融合**（libzstd 模型）：序列解码后立即执行，不再经序列向量往返。
  注意纯融合曾 -1.8%，RLE 伪装单状态表（fake entry 自转移、acc_log=0）消掉
  3 个 Option 寄存器 + 每序列 6 分支后才翻正，同时删除 4 个变体循环。
  json.zst1 +10%（反超 zstd crate slice）、json.zst3 +7%。`971b43f`
- 状态携带 decode_step + 指针游标执行器 + `HEADROOM` const 特化（流式缓冲保证
  块大小 +16B slack 时，每序列预算门与 wildcopy 门整体消失）：流式 +4-7.4%。
  `c6e5fd9`
- 循环只携带三个 FSE states（非 packed entries），`bits`/`src_len` 出流状态
  （reload 恒可从内存重建位窗口）；执行器游标裸指针化折叠 base。
- **寄存器墙定律**：该循环 ≥15 个活跃值 LLVM 即把位窗口栈驻留（annotate 上
  `shlx` 带内存操作数即中招）。所有结构改造（拆批处理、两阶段流水线、寄存器化
  repcode）均证伪——融合结构局部最优，见[已证伪方向](../negative.md)。

## 序列解码

| 技术 | 效果 | commit |
|---|---|---|
| 64 位回读位流（指针 + bits_consumed，reload 内联）+ 打包 u64 FSE entry（base/add_bits 建表时并入，单 load 全字段） | 序列解码 ~15% | `0e88ecc` |
| 三 packed 表合并单基址数组 + 常量槽偏移（LL512/ML512/OF256） | 消每迭代表指针重载 | `0d12538` |
| 预移位位窗口（`win = bits << consumed`） | 每读只剩两条移位 | `0d12538` |
| 成组批提取：add-bits 三合一 + 状态转移三合一，单次串行窗口读并行拆分（sum≤31 免 mid-seq reload） | skewed.zst3 +7-8%、json.zst3 +3-5% | `c79086f` |
| 零宽无分支读（`wrapping_shr`/bzhi 掩码）+ repcode 解析轮转全 select | skewed.zst3 流式 +4.5% | `1fdcfaa` |
| BMI2 运行时分发（共享 `#[inline(always)]` impl + `#[target_feature]` 包装） | Zen4 中性、Intel 受益 | `72d65a1` |
| FSE 建表逐符号 spread 预计算（SymbolSpreadInfo 内联数组） | 建表 profile 4.9%→1.9% | `59f6c73` |
| `do_offset_history` 槽索引化（code-1+ll0 单值定槽+轮转，穷尽等价测试背书） | 每序列指令下降 | `82f3a00` |

## 执行器（libzstd wildcopy 拷贝栈移植）

- literals：≤16 内联 copy16（`literals_buffer.reserve(16)` 保证越读不越界），
  \>16 走 16B 块循环。
- match：offset≥16 → copy16/16B 循环；[8,16) 8B 循环；<8 overlapCopy8
  （dec32/dec64 表一次写 8B 把有效 offset 撑到 ≥8；**前 4 字节逐字节**让 store
  喂 load）。
- 预算门 `w+ll+ml+16 <= out_len` 保证超写落在合法区，尾部走精确慢路径。
- 动机：json/skewed 是海量 5-6B 小拷贝（LD_PRELOAD 实测 32MiB ~3M 次 PLT 调用，
  占 ~35% 时间）。落地后 memmove 14.3%→1.6%，json/skewed/text 解码 +23-45%。
  `8ce527a`
- active 段匹配源代数化简 `dst - offset`（虚拟距离=物理距离，base 间接层消掉）；
  wrapped 源 #[cold] outline 甩掉 4 个视图活跃值；边界检查三合一。`bee924d`
- prev-segment 线性快路径：wrapped 前段源物理上位于 `dst + seg_a_end - offset`
  （高于写游标 ≥MAX_BLOCK，wildcopy 双侧超写安全），一次 wildcopy 替代通用段游走；
  **放进已有 out-of-line callee 保热循环 codegen**。skewed.zst9 +4.8%。`6bccc54`

## Huffman literals

- X2 双符号表（libzstd HUF_decompress4X2 移植）+ 自定 **80% 成对门槛**（按权重算
  可成对码空间占比，不足留空走 X1）：skewed.zst1 +43%、skewed.zst3 +15%。`0af7fc1`
- X1/X2 流状态指针化（ip/op 折叠 base，活跃值 16→14）：内循环指令 -10%。
  **结论：已到实际下限**——libzstd 手写 asm 3 次 dtable load（memport-bound
  ~0.7 cyc/B）vs 我们 1 load+2 shift（ALU-bound ~0.75 cyc/B），同量级。`5be5f88`

## 杂项

- 无 Content_Checksum 帧不初始化/更新 xxh64（+2.7%）；校验和计算树内化（编解码
  共用 8 链实现，twox 出运行时依赖）。`5e2c99f` `81d2119`
- Predefined 模式连续块不重建 FSE 表（消除每块分配，+2.5%）。`6d40e4f`
- StreamingDecoder `read` 解码至填满调用方缓冲（去 UptoBlocks(1) 粒度，+2%）。
  `26a29da`
- 允许过冲的定宽拷贝（`copy_bytes_overshooting`，u128/usize 整字循环绕开 memcpy
  长度分派）；指数倍增重叠拷贝 `repeat_in_chunks`（log2 次 memcpy 代替 ml/offset 次）。
- interleaved huffman 表访问/输出 unchecked（边界检查已被分支预测消化，≈0 但保留）。

## 残余（对 libzstd streaming，json/skewed 1.05-1.3×）

融合循环已贴串行链极限（IPC ~3.7）；残余是循环指令质量：entry 解包 6+ uop/序列、
shlx/shrx 串行链、~20 活跃值残留溢出。寄存器墙已证（见 negative），进一步压缩需
decode_step 本体重构；下一量级需算法级变化（libzstd 式 8 深序列环形缓冲流水、
SIMD 拷贝调度），收益/风险比待评估。
