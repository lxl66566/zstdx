# 踩坑记录 · 解码侧

## 已修 bug（模式可复发）

- **X2 跨块残留**：Compressed 块选 X1 时不清 x2 表，后续 Treeless 块拿过期表走 X2
  → BitstreamReadMismatch。修：`build_table_from_weights` 开头 `x2.clear()`。
  另：X2 选择需 80% 成对门槛——text 的 tl=11 表仅 55-61% 可成对，无门槛曾致 -2.7%。
- **flat 流式调试四连坑**：① grow 后不复查空间就 return → 越界 panic/segfault；
  ② doubling 锚点误写成随 copied 滑动 → overlap match 读到未写字节（锚点必须固定，
  chunk ≤ readable+copied）；③ 双重偏移——`out = buf[end..]` 切片指针已偏移，match
  源又用绝对偏移，需恢复物理基准（slice 路径 block_start=0 巧合正确，掩盖了问题）；
  ④ wrap 时 `origin += end` 而非 `= end`，否则虚拟坐标回退（两代 FlatView 各修一次）。
- **wildcopy 三坑**：① overlapCopy8 前 4 字节必须逐字节（offset<4 时 store 喂
  load），u32 整读坏文件；② wildcopy 助手首次用 (输出相对 dst, 物理 src) 两套坐标
  算距离 → `d - s` 混空间，改为全指针（offset≥16 的分支不依赖距离，多数文件侥幸
  通过——恰是难查的原因）；③ 测试走 `decode_blocks(All)`（state.flat 会 wrap）而
  `decode_all_to_vec` 不 wrap——**复现必须用测试的真实路径**。
- **overlap_copy8 下溢**：`s2 + (8 - dec64)` 在 spread offset 5-7 时调整值为负，
  usize 下溢——debug panic（挂 12 个语料测试）、release wrapping 侥幸正确。改有
  符号表 + `offset()`，对齐 libzstd `*ip -= dec64table[offset]` 语义。
- **MT 解码 literals 计数**：收尾校验用缓冲总长而非增量——ST 每块 clear 无事，
  MT stage A 按段累积 → 段内第 2+ 个 huffman literals 块全误报，11 个语料文件
  7 个 MT 解码失败而仓库测试（textish 语料，Raw/RLE 字面量侥幸通过）全绿。
  **教训：仓库测试语料形状会系统性掩盖某类 bug**；新增 semi-structured 回归测试。
- **decode_to_vec_mt 串行回退容量**：并行路径逐段增长，回退路径对空 Vec 报
  TargetTooSmall；翻倍重试必须**显式跟踪翻倍值**——`reserve(cap)` 在 spare≥cap 时
  是 no-op，否则死循环（调试时一度误判 flat 解码器死循环，实为重试循环每轮重跑
  完整解码）。
- FSE 逐符号预计算的 assert 误报：幂概率符号 `nb_d` 可能 = acc+1 但无 double 状态、
  永不被使用 → 断言应为 `num_double == 0 || nb_d <= acc`（第一版因此回退重做）。
- `debug_assert_eq!(written, before)` 参数写反，挂 3 个 debug 测试。
- corruption_smoke 损坏命中帧魔数时 panic：BadMagicNumber 是合法错误，示例应
  let-else 返回而非 unwrap。
- bench_files 的 strip_suffix 只认 `.zst`，`.zstN` 会拿压缩文件当参照报"校验失败"
  （已修：zstdx-bench `files` 按任意 `zst*` 后缀剥扩展名并尝试裸 stem 与 `.raw`）。
- **流式单帧即止 vs flat 解所有帧**：StreamingDecoder 首帧 `is_finished` 后 read
  返回 Ok(0)，`decode_all` 却循环解所有帧——同一多帧输入两路径结果不同（fuzz
  decode target 的新对拍断言抓到；旧 target 只 read_to_end 从未暴露）。语义以
  libzstd/zstd crate 实测为准：两路都解所有帧、透明跳 skippable、尾部任何垃圾
  字节（≥1，含 1-3 字节部分 magic）都报错、空内容帧不得当 EOF。修：帧尾续帧
  原语（预读 4 字节 magic 区分干净 EOF 与部分尾巴）下沉 `decoding::frame_source`，
  `stream::read::Decoder` 与 StreamingDecoder 共用；**续帧判断必须在 read 的填充
  循环内**而非仅入口——空帧结束当次 read 时 can_collect==0 会误报 EOF。
  另注：该 crash 第二帧 FHD=0x38 置保留位，libzstd 拒绝而 zstdx 宽容接受——
  帧头严格度差异是独立问题（见 todo）。

## profile 与归因

- **AMD 分支 miss 采样 skid 极大**：`ex_ret_brn_misp` 样本落在基本块中间（前后
  ±10 条无法归属），ibs_op 无 miss 过滤——**归属靠 stub 二分 + perf stat 总量**，
  不信 annotate/script 的 miss 归属。
- **stub 二分纪律**：stub 后必须校验 exit code + 输出长度（曾把越界读段错误的半程
  残值当数字，cycles 假降 13×）；stub 要保真实偏移（`%4096` 小偏移会让访存模式
  全变、wrapped 全消失，混杂无效）。
- **拷空 exec 归因法**：依次 stub 掉拷贝循环分解 miss 来源——解码侧仅 10%，
  执行器（拷贝循环+门控）占 88%，推翻"解码侧分支是大头"的旧假设。
- **10GB/s 级负载（text.zst3/z9）墙钟不可信**（轮内 0.82-1.25）：看 cycles 或只信
  <9GB/s 负载的交错中位数。
- **.st 热路径可能是 WRAP 实例化**：流式 2MB 后 wrap，携带全套视图值（每序列 ~12
  次栈往返）；cold outline 后仍可能有残留溢出（活跃值 ~20 > 15 GPR）——annotate
  前先确认热的是哪份实例化。
- perf 揭示 xxh64 absorb 8.15% 后排查排除：8 链 ~20GB/s 已快于 zstd 标量 4 链，
  非差距来源——热点高 ≠ 有肉，先对照已知极限。
