# 性能比较 · 当前快照

> 对比对象：zstd crate 0.13.3（libzstd 1.5.7 binding）。所有数字来自交错 A/B；
> 各条目**数据时点不一**（括号内标注 commit），完整原始表见[广域矩阵数据](matrix.md)。
> ⚠️ 广域矩阵最后一次完整测量在乱链修复前后（`b1dd010`/`4ff2b7b`），其后落地了
> optimal parser、u32 表项、MT ratio 保持、stride-3 prefill、流式 MT 等，编码侧
> 多数格子已过时；**矩阵重测本身在[待办](../todo.md)首位**。

## 一句话结论

- **解码**：bulk 全胜（注意 zstd 的 bulk 包装本身慢）；流式 text/random/zeros 领先
  或持平，json/skewed 落后 1.05-1.4×。
- **编码**：text/skewed/zeros 多档速度领先；json 低档落后 1.3-1.8×；
  ratio 已全档对位（Best/Opt 反超、Ultra 基本持平），但 Best/Opt/Ultra **速度**落后。
- **MT 编码**：速度 2-4.5× 领先 zstd-mt；ratio 崩塌已修复（`a37ebaa`），mt16 各格
  回到 ST ±0.5%。
- **MT 解码**：独占维度（libzstd 无），但暂为负资产（0.66-1.0×），stage B 并行化是
  最大待办。
- **流式编码 MT8**：json 类 0.29-1.11×，text 类持平至 2.2× 快，best 档 2.4× 落后。

## 解码（时点：`b1dd010` 矩阵 + 各批次交错 A/B 累计）

| 场景 | 状态 |
|---|---|
| bulk（slice API） | 11/11 全胜（xslow 0.22-1.00） |
| streaming json | 落后 1.28-1.39×；D3-D5 批次后累计收窄至 ~1.2× |
| streaming skewed | zst1 1.09× / zst3 1.23× / zst9 1.40×；收窄至 zst9 ~1.3× |
| streaming text | 1.05-1.22×（zst9 已近追平） |
| streaming random / zeros | 0.80×（反超）/ 1.00× |
| MT 解码 | 无扩展性：json mt8 打平 ST，skewed.zst9 恒 0.66×（stage B 串行占 45-80%） |

## 编码 ST（bulk）

各格取已知最新（时点不一，绝对值不可跨行比较）：

| 格 | 速度 | ratio | 时点/出处 |
|---|---|---|---|
| json.Fastest | 1.78× 落后（476 vs 849 MiB/s） | 6.00-6.20 vs 6.11 | `b1dd010` / `aa07308` 后 |
| json.Fast | 1.35× 落后（350 vs 472） | **5.33 反超** zstd-3 的 5.29 | `b1dd010` |
| json.Balanced | 乱链修复后 1.41× 落后（133 vs 185）；`a37ebaa` prefill 后 ratio **5.66→6.08**（反超 zstd-9 的 5.76） | ↑ | `4ff2b7b` / Changelog `a37ebaa` |
| skewed.Balanced | `a37ebaa` 后 42→2139 MiB/s（ratio 1.86→2.00 反超） | ↑ | Changelog `a37ebaa` |
| json.Best | 0.69× 领先（82 vs 56）→ 换 opt 核后 12-19 MiB/s，ratio **6.88 反超** zstd-12 的 6.14 | ↑ | `4ff2b7b` / `b39a192` |
| skewed.Best | 0.67× 领先；ratio 1.996 反超 zstd-19 的 1.84（opt 核后） | ↑ | `4ff2b7b` / `b39a192` |
| text.Fastest/Fast | 0.85× / 0.67× 领先 | 298-329 vs 308-333 | `b1dd010` |
| skewed.Fastest/Fast | 0.44× 大幅领先 / 1.44× 落后 | 2.00 持平 | `b1dd010` |
| random 低档 | 1.18-1.46× 落后（raw 块逐块开销，待 profile） | 1.00 持平 | `b1dd010` |
| zeros 全档 | 0.02-0.27× 大幅领先 | 32k 持平 | `b1dd010` |
| Opt/Ultra | json Opt ≈ zstd-16（7.8 MiB/s）；text Opt 313 vs ~440 落后 | json Opt **7.57 反超** 7.28；json Ultra 7.52 vs 7.49 持平 | `c726dfd` |

历史战果（vs zstd crate L1，ENCPERF 终态，Windows 原生相位）：json 287→447 MiB/s
（比率逼近 6.00 vs 6.11）、skewed 434→2951（反超 2.35×）、text 2500→9480、
zeros 4500→18573（反超 1.41×）、random 1120→2328（含校验和反超）。

## MT 编码

- 速度（`b1dd010`，冷线程池对冷线程池）：mt4 起全面领先，mt8/16 达 2-4.5×
  （json.Fast mt16 2981 vs 980 MiB/s）；mt32 回退（调度颠簸，job 收益见顶）。
  zstd mt16 warm-pool 参考 1512 MiB/s，仍慢于我们冷池。
- ratio：text 类周期语料曾崩塌（328.8→19.7 @mt16，zstd-mt 189）。
  **已修复**（`a37ebaa`）：overlap=window + 条带 prefill 索引 + gain 门 + job 起点
  周期种子；周期语料三档 mt/st 2.7-2.8→1.00，32MiB 全语料各格回到 ST ±0.5%。
  ⚠️ 矩阵 §4 的 MT 表作废，重测在待办。

## 流式编码 MT8（时点：`27b91cf`）

ruz mt8 vs zstd stream-mt8（x = 我方时间/对方时间，<1 快）：

| 格 | x | | 格 | x |
|---|---|---|---|---|
| json.fast | **0.29** | | text.fast | 0.46 |
| json.balanced | 0.52 | | text.balanced | 1.04 |
| json.fastest | 1.11 | | text.fastest | 1.00 |
| json.best | 2.45 | | text.best | 2.40 |

best 档落后是底层 Best 档编码速度差距（流式已达自身 bulk 上限 79%/60%），非管线问题。
对自身流式 ST：json 类 3.7-3.8× 加速；text 快档 0.55-0.68×（高冗余数据 ST 近免费，
zstd 同病）。

## 其他口径

- 小语料（vs zstd crate bulk L1，ENCPERF）：skewed-1K 6.3×、random-64K 0.93×
  （从 0.30× 追平）、random-1M 0.99×；json/text 的 64K/1M 仍落后 ~2×（待办 P1）。
- 未知尺寸流式编码 text.Fastest：zstd 输出 1.84MB（ratio 18.2，bulk 308.9），
  我们保持 112K（298.7）——同一 API 形状下 16× 压缩率 + 3.8× 速度双胜（`b1dd010`）。
- 校验和开销（hash on/off）：json.Fast 1.036、text.Fast ≈1.05；sidecar 卸载后
  random/text/skewed 追平或反超 no-hash（ENCPERF）。
