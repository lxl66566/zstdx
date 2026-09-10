# 性能优化 · 编码侧（匹配器以外）

> 匹配器与级别策略见[匹配器与压缩级别](matchers.md)；多线程与流式管线见
> [多线程与流式](mt-stream.md)。

## 入口与状态管理

- **零拷贝 slice 入口** `compress_slice_to_vec`：匹配窗口直接借用输入（裸指针窗口
  + unsafe Send），消读 pass、窗口 compaction 与每调用窗口分配；输出 upfront
  reserve，块直写最终输出。text +40%、zeros +60%。`51b019e`
- **thread_local 状态池**（哈希表/默认 FSE 表/BlockScratch，take() 式跨调用复用、
  嵌套安全）：1KiB 载荷 3-7×，4KiB +30-80%。`cb02c32`
- **emit 路径重构**：熵表按值 take/restore 替代每块深克隆 3×FSETable+Huffman
  （原每块几百次 alloc 是最大黑洞 ~30%）；`start_matching_into` 缓冲直写替代回调
  闭包（闭包捕获 2×Vec + matcher 状态 → 寄存器溢出，一条 store 采样占 40%）；
  matcher 窗口读免检。分项 +2%/+2%/+11%，累计 json +24%、text +39%。`68ae651`
  经验：**免检读取收益（+11%）远大于结构改造（+2%×2），bounds check 逐字节路径
  是首要目标**。
- 块内容原地写 frame output（先占 3 字节块头，压完回填）+ literals/序列/预计算码
  缓冲进 BlockScratch 池：random -6.8% 指令。`95b2006`
- 零序列块 literals 免暂存（matcher 跳整块拷贝，编码器直读窗口）：random
  64K-1M +13%。`15d127d`

## FSE

- SoA 平铺表（probs/start/transitions 连续内存）替代 256×Vec + 双重排序。`81afece`
- 逐符号 spread 预计算；平面转移表 `(next<<13)|(nb<<9)|baseline` O(1) 状态转移 +
  LL/ML code LUT：json 189→245 MB/s。`5619e80`
- 每序列 6 次 write_bits 合并为 2 次 u64 拼写（转移位 ≤27bit、add-bits ≤51bit 各
  一次）：再 +4%。`bbde2ea`
- 转移位与 add-bits 合并单次 hot_push（合计 ≤56bit 安全上限，超限回退双 push）。
  json -0.4% 指令。`2d18813`
- **表模式选择**（libzstd `selectEncodingType` fast 分支移植）：RLE 单码/Predefined
  阈值/Encoded + normalizeCount M2 归一化 + optimalTableLog + 末序列计数减一。
  json 比率 5.22→5.37。`1a17db3`
- **repeat 模式（mode 3）**：前块表覆盖全部活跃码且位成本估计（Σ c·log2）≤ 新表
  描述 + 熵下界时整表复用、零描述字节；Predefined/RLE 选择作废记忆表
  （`PrevTable::{New,Keep,Clear}` 三态防与解码器表状态失配）。json -2.2% 指令、
  比率 5.99→6.00。`7769bf8`
- RLE 退化单态 FSE 表：table_size=1 与正常模式完全同路径，零特判分支。

## Huffman

- **boundary package-merge 最优限长码**（Larmore-Hirschberg）替代初版的秩权重
  阶梯（旧方案依赖 count 顺序不依赖大小，skewed 直方图上损失大）：全级受益，
  json Ultra 7.33→7.52、text fastest 196.6→206.0。`aa07308`
- 标量 4 符号批量累加：packed u16 `(code<<4)|nb` 码表，先腾位至 <8bit 无分支累积
  4 符号进 u64，单条非对齐 store 刷整字节。skewed +71%、json +4%。
- **AVX-512VBMI 4-bit 码打包内核**（均匀 9..16 符号字母表）：64 符号/批，
  `permutex2var_epi8`×2 解 256 项 LUT + 两次 permute 反排配对 nibble。
  skewed 指令 -48%、吞吐 +43%。`2be2207`
- RLE literals 模式（literals 全同字节 → type1 头 + 1 字节内容）。
  坑：5 位尺寸格式的 size_format 只占 1 bit（见踩坑）。

## 直方图与不可压检测

- literals 直方图 **4 路子表**（按位置 mod 4 分流，避同计数器 store-forwarding
  串行化）：skewed cycles -7%、吞吐 +10%。
- ≤16 字母表 **AVX-512 精确直方图**：16 槽 `cmpeq_epi8_mask` + popcount 累加，
  popcount 总和兼职第 17 符号检测（未污染才可回退标量）。skewed 指令 -18%、+28%。
  `e76bb60`
- **Miller-Madow 熵采样拒绝门**：strided 采样（~1024 点）+ Miller-Madow 偏差修正
  + distinct>208 预筛 + sticky `gate_hold`；≥8 bits/byte 才拒绝。
  random 指令 -43%、+43%（1450→2060，反超 libzstd L1）。`cdc70c0`
- literals 熵下界预检（精确直方图 + Shannon 界 + 8% 余量，无望胜 raw 则跳过
  Huffman 尝试；直方图与建表共享一次扫描）：random 455→1178（+159%）。`a32d2c1`
- 三表直方图单趟融合（一次遍历 packed codes 更新 ll/ml/of 三张计数）。`6690a05`
- no_std 下 log2 用线性尾数近似（误差 <0.086 被 8% 余量吞掉）。`9121215`

## 校验和（xxh64）

- **自研 8 链 XXH64**：每迭代 2×32B、8 条累加链在飞（4 链 +8%）；解码端共用，
  twox-hash 降为 dev-dep，运行时零外部依赖。`3c0bf97` `81d2119`
  注意：AVX-512 vpmullq 向量核是负结果（串行依赖链），8 链标量交错才是正解。
- **pass fusion**：RLE 均匀扫描一边比较一边跑 XXH64 round（均匀块哈希在唯一一趟
  内完成；失配返回 32B 对齐 resume offset 续接）；raw 块拷贝边拷边吸收；
  零序列块门拒绝早退。zeros +22%、random +10%。`3c0bf97`
- **sidecar 线程卸载**（std+hash、输入 ≥256KiB、≥2 核）：thread_local SPSC 环
  （512 槽）投递块字节，专职 worker 消费；`BlockChecksum` trait 统一 inline/offload。
  random +13%、text +26%、skewed +14%、zeros +2%、json +4%。`df33295`
- worker **有界自旋停泊**：~300µs 预算后 condvar 停泊（80µs 不够——raw 块管线是
  worker 饱和节奏，唤醒延迟直接落关键路径）；head 翻转在锁内发布防丢失唤醒。
  饱和运行每帧仅 ~1 次自愿切换、空闲 0 tick。`70b5fd4`
- MT 路径校验和在调用线程内联并行吸收；流式 burst 期间主线程 hash。

## 位写入与杂项

- BitWriter 64 位 flush 用单条非对齐 u64 store 替代 8 字节 memcpy：全语料 +1-2%。
- 序列码单次计算共享（choose_table 与 encode_sequences 消费同一组 code 数组，
  META 查表拆 add-bits）：输出位相同、代码更干净。`b0c2653`
- 序列码/加位单趟预计算（codes 打包 u32 流、add-bits 预合并 u64）：json +3%。
  `d9a840b`
- 匹配器直推 packed 序列流（`Matcher::start_matching_codes`，json -3.5% 指令）
  ——详见匹配器页。`6faba75`
- RLE 检测 u64 分块（原闭包逐字节索引无法矢量化）；RLE 块 skip_matching 只插首
  位置（均匀游程所有 5 字节窗口同槽）：zeros 7.3×。`6abb9ae` `d9f078a`
- 匹配发射后 short-literal 越界覆写拷贝（≤8B literal run `reserve(8)` + 非对齐
  读写 + set_len，读越进后续 match、写溢 ≤7B 由后续 push 覆盖）。
- uniform 块检测 4×u64 批比较 + 刻意 `#[inline(never)]`（内联进大调用者会让循环
  丢数据指针、每 32B 从栈重载，吞吐减半——反直觉但有实测支撑）。
