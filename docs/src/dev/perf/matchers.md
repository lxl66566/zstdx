# 性能优化 · 匹配器与压缩级别

## 策略阶梯（现行）

| Level | ≈zstd | 策略 | 关键参数 |
|---|---|---|---|
| Uncompressed | 0 | raw 块 | — |
| Fastest | 1 | fast：hash5 单探测表 | 窗 768KiB、表 2^15 u32、ml≥6 门槛、miss 步 `(miss>>2).min(255)`、rep0 前探 + rep1 链 |
| Fast | 3-5 | dfast：hash8 2^17 + hash5 2^16 双表单探测 | 窗 1MiB、匹配后 4 锚点补插、无状态 anchor miss step |
| Balanced | 6-9 | hash chain + lazy | W1MiB / H20 / depth 8 / lazy、u32 表 |
| Best | 10-15 | optimal parser 低配 | 16 compares、targetLength 32 |
| Opt | 16-17 | btopt（完整移植） | 1 MiB 窗 |
| Ultra | 18-22 | btultra(+2) | 2-pass 首块统计播种 |

## Fastest（fast）

- **zstd-fast 风格重写**：连续窗口 + 单探测 u64 哈希表（原 `epoch<<48|pos` 槽，
  reset 只增 epoch 免清表；后随 `327bc99` u32 化退役 epoch）、hash5（u32 + 第 5
  字节）、u64 块比较前/后向扩展（后向扩展进 pending literals）、miss 步长递增。
  匹配 ~3×，text 比率 54 倍跃升（旧 matcher 在长重复数据上近乎失效）。`a2cc982`
- **repcode**：matcher 维护 rep[3]（与解码端共用 `do_offset_history` 语义）；
  ll==0 用 **pos+1 探测**（当前字节充当那 1 个 literal，后向扩展保护
  `start > anchor+1`）；发射 of_value（1=rep0、新偏移=offset+3）。
  json rep 命中率 0.07%→7.6%。`1d25a26`
- rep1 即时链探测（zstd rep_offset2 循环，ll==0 自动交换 rep0/rep1）：
  json 比率 4.93→5.23。`49de097`
- **稀疏索引**：≤16B 全插、>16B 只插 start+2/end-2 双锚点（比纯 zstd 双锚点多保住
  json 远距匹配；纯锚点 json -2.1%/text -6.3% 比率）。json +6%。`4e79929`
- 表 2^16→**2^15** + miss 路径隔位插入：小表高竞争 + newest-wins = 强近距偏置
  （zstd -1 是 2^14 槽配 512K 窗 = 32:1 覆盖）。`21fb329`
- hash 路径 **ml≥6 门槛**：5 字节 match 的序列成本 ≈ 其覆盖 literal 的成本，拒绝它
  让扫描推进到更长 match 起点（试过 7/8：json 饱和、text 崩，6 是平衡点）。
  json 5.44→5.72。`be2bfe9`
- 窗口 448K→**768K**：text 语料 tile 周期落在 512K-768K，跨 tile 匹配解锁。
  text 比率 6.46→239。`7a2a388`
- **探测 select 化**（libzstd selectAddr）：无效候选 select 成扫描位置本身，字节
  比较 + `cand != ip` 单分支拒绝，替代 epoch/win_base/距离三连比较；rep0 预探折叠
  为 `probe >= win_base + rep[0]` 单比较。json 455→466 MiB/s。`29bcb02`
- `TableEmit` 上下文结构替代 12-14 参数自由函数（防参数栈溢出）；扫描表访问裸指针。
  `c528c57`
- 输出位流侧：u64 拼写位写（见编码侧页）；比率收官 json 6.00（vs zstd-1 6.11）。

## Fast（dfast，libzstd level 3 对位）

- **错档教训**：Fast 最初用 chain+lazy（zstd-3 实为 dfast）——2.5MB 热随机访问表 +
  逐位插入 + lazy 双倍搜索，json 2.73× 落后且 text 3× 慢于 Fastest。dfast 移植后
  json +80%（175→314 MiB/s，ratio 5.47→5.33 反超 zstd-3 的 5.31）、text +154%。
  `19077f3`
- 骨架：双表（hash8 2^17 prime 0xCF1BBCDCB7A56463 + hash5 2^16 复用 chain 缓冲）
  先插后探、两位置流水线、短命中后 ip+1 长探升级、后向 catch-up、repcode ip+1 预探、
  **匹配内部完全不插表**、匹配后 4 锚点补插双表 + 立即 repcode 循环。
- **发射路径瘦身**：`SeqWord` 16B 单流（codes/add_bits/add_nbs 三流合一，3 push→
  1 push；push/pack 必须 `#[inline(always)]`，仅 #[inline] 不内联仍占 6.8%）；
  ll==0 序列（json 占 50%，rep 链）跳过字面量拷贝；探测 cmov 化。
  json 314→366 MiB/s。`1398d23`
- **miss step 无状态化** `1 + (ip-anchor)>>8`（去每 256 跳的计数器对）+ backfill
  精确谓词（"match 覆盖了探针位才补插"，替代 step<4 代理）：json +1.7%、text +2.6%。
  `0b46f21` `06b67dc`
- 窗口 1MiB（2MiB A/B 反而双输：远距候选抢占近距更优匹配的单探测槽）。

## Balanced / Best（hash chain）

- chain 位置索引表 + lazy（hash-log/chain-log/depth/lazy/窗口参数化，单
  `ChainMatcher` 复用 emit/extend/repcode 逻辑）。`f8cc66d`
  移植坑：repcode 向后扩展必须保留 ≥1 字面量；pos==anchor 必须探 rep（否则输自家
  Fastest 8 倍）——细节见踩坑。
- **漏配加速**：不可压 run 的 miss 加速（长字面游程步进爬坡），random 全档 ~2.1
  GiB/s，Best 快过 zstd-12 的 918 MiB/s。`47b60b0`
- **reach=window 原则**：chain 表按位置索引，chainLog 即匹配 reach；`reach<window`
  （C18/C19/C17）在 skewed 上崩盘——远距离重复丢失 → 匹配变短 → 逐位扫描变慢。
  Balanced 定格 W20+C20（同 9MB 表却全面变好：淘汰 >1MB 的高成本 offset 编码）。
  `3c88244`
- **乱链修复（重大）**：insert 用窗口索引、walk 用绝对位置，win_base 一推进就跑在
  截断乱链上（有效 depth ~2-5）——此前 Balanced/Best 的"速度领先"是少烧 probe 的
  假象、ratio 亦偏差；流式 Best 18 vs bulk 74 MiB/s 的 4× 崩坏同源。修复后按真实
  链路重调全梯 + **beat-check**（候选 4 字节 beat 检验免全扩展）。`36203c1` `4ff2b7b`
- **Best 换核**：depth 16→64 ratio 仅 +0.9% 而速度差 2.7×——深搜性价比极差，Best
  改跑 optimal parser 低配（16 compares / targetLength 32）。json Best 5.77→6.88
  （反超 zstd-12 的 6.14）、text 270.8（反超 256.1）。`b39a192`
- **表项 u32 化**：存 `abs+1` 低 32 位（0=未写哨兵），读取按扫描位置重建高位、回绕
  一个 4GiB 周期；stale 条目靠窗口域校验 + 字节验证兜底。表工作集减半（Balanced
  16→8MiB 对齐 libzstd），json.Balanced +18%、text +20%。epoch 机制退役
  （opt parser 保留 epoch u64）。`327bc99`
  对比：Fastest 2^15 表（驻 L2）时 u64→u32 无效——**表项宽度只在表超出 L2 时是
  杠杆**。

## Opt / Ultra（optimal parser，libzstd zstd_opt.c 全量移植）`c726dfd`

- lazy 填充的二叉匹配树（insertBt1 / insertBtAndGetAllMatches，epoch 标签绝对位置）；
  前向 DP over stretches + 定点分数位价格（×256，highbit+线性插值近似 -log2(p)）；
  跨块频率自适应（rescaleFreqs 衰减 + updateStats）；btultra match+1-literal 复查；
  2-pass 首块统计播种（epoch bump 使 pass-1 树失效）。
- 正确性关键细节：`ZSTD_count` 返回指针差（续数要**替换**不能累加）；rep history
  由路径遍历**每 series 更新一次**（发射侧 per-sequence 更新是解码器等价语义，二者
  取一）；插树计数 cap 在 4096 位 DP 窗——否则对 parser 跳过的区域（远偏移长匹配）
  按 stale head 重计数，text 45→313 MiB/s。
- 战果：json Opt ratio 7.57 反超 zstd-16 的 7.28；json Ultra 7.52 vs zstd-19 7.49
  （同窗 1MiB 对比 7.53）；text Opt 271.5 vs 272.0。速度：json Opt ≈ zstd-16，
  text Opt 313 vs ~440（落后）。

## 调参经验（跨级通用）

- **比率与表压强强耦合**（Fastest）：任何提高插表量的改动（全密度探测、慢斜率）
  都让 json 比率崩（5.5 级）；双锚点把长匹配插表量降到 1/8 后表压强才有余量。
- **depth 性价比极差**：json d16 vs d64 ratio 仅 +0.9%、速度差 2.7×。高端 ratio
  的正路是 opt parser 而非更深 chain。
- **哈希表要匹配数据**：skewed（16 字母表）H17 每槽 ~256 链永远走不满，H20/H21
  让链尾终止（zstd-9 是 H21）；H18 反而在 skewed 崩（head 桶更细 → chain 头部
  命中变稀）。
- **周期语料锁定靠 rep0 直探不是哈希表**：固定网格在聚簇 5-gram 上数学无解
  （孪生槽位同值递归掩埋 / 相位对齐陷阱）——MT job 起点种子机制由此而来。
- **gain 门**（store 决策 `ml*4 ≥ ilog2(offset)+7`）：C 的 +7 是"替换门"不是
  store 门；我们的表比 C 密（stride-1 prefill + walk 取最长），远距弱匹配选拔偏差
  更大，门刻意更严。代价：text 中短距匹配误杀（T=7 时 -0.55%）。
- **W4M 双输**：4M 窗只找到更远而非更长的匹配，offset 码位损失 > 长度收益。
  扩窗必须配 LDM。
