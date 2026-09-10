# 引言

## 本手册是什么

`ext` 分支（zstdx 的深度优化 fork）的开发记录手册：把根目录十来份过程文档按主题
合并、去重、校对后沉淀为结构化知识库。目标是**好补充、好查询**——之后每批优化做完，
把结论归档进对应主题页，而不是新开一份流水文档。

## 与其他文档的分工

| 文档 | 定位 | 更新方式 |
|---|---|---|
| `Changelog.md`（git 内） | 面向上游的逐条变更记录 | 每笔提交附条目 |
| `docs/`（本手册） | 主题化经验：技术方案、完成清单、待办、负结果、踩坑、性能比较 | 每批工作完成后归档 |
| 根目录 `*.md`（不进 git） | 过程流水：方案、进度、A/B 原始数据 | 已归档，保留作存档，随时可删 |

## 阅读约定

- **轮次编号已忽略**。原文档的"第 N 轮 / rN / 第 N 批 / M1 / D6 / E9"编号跨文档
  交叉重叠且不连续，本手册按主题组织，只在因果有意义处保留先后关系。
- **所有改进项都对照过 Changelog**（截至 `27b91cf`，2026-09-10）：过程文档里列过
  但已落地的项不再出现在[待办清单](dev/todo.md)；待办每项标注来源与现状。
- **性能数字都标数据时点**。测试机（AMD Zen4 32C）噪声 ±10% 且有慢漂移相位，跨
  时段绝对值不可比，比较性结论只认交错 A/B。数字出处一般是 Changelog 对应条目或
  广域矩阵（后者时点较旧，见 bench 页说明）。
- commit 短哈希可用 `git show <hash>` 查看细节。

## 术语表

| 术语 | 含义 |
|---|---|
| Fastest / Fast / Balanced / Best / Opt / Ultra | 编码级别阶梯（≈zstd 1 / 3-5 / 6-9 / 10-15 / 16-17 / 18-22） |
| fast / dfast / chain / opt(bt) | 匹配器策略：单探测表 / 双表 / hash 链+lazy / 最优解析（二叉树+DP） |
| ST / MT | 单线程 / 多线程 |
| bulk(slice) / streaming(st) | 一次性内存 API / 流式 API |
| xslow | 我方时间 ÷ zstd 时间，>1 = 我们慢 |
| 交错 A/B | 双方逐轮交替执行取中位数，均摊机器漂移 |
| restart point | 熵状态自描述的块边界（MT 解码分段点 = zstd `-T` 的 job 边界） |
| wildcopy / overlapCopy8 | libzstd 式定宽超写拷贝 / offset<8 的重叠拷贝位技巧 |
| SeqWord / packed 序列流 | 匹配器直推的 16B 单流序列编码表示 |
| rep0/rep1/repcode | zstd repeated offset 机制 |
| gain 门 | chain store 决策 `ml*4 ≥ ilog2(offset)+7` |
| 语料五形状 | json（半结构化）/ text（源码平铺）/ skewed（16 字母表）/ random / zeros，各 32MiB |
| dump 对拍 | 全级别输出字节级快照对比，"不该改输出的改动"的免费回归探针 |

## 源文档 → 手册映射

| 原文档 | 内容 | 归档去向 |
|---|---|---|
| 早期优化.md | 第 1-3 轮合并总结（解码为主 + 编码 matcher 重写） | perf/decoding、perf/matchers、pitfalls、negative |
| 初期编码优化.md | 编码端第 4-6 轮 | perf/matchers、perf/encoding、pitfalls |
| ENCPERF.md | 编码端 r11-r15 + 总收官 | perf/encoding、bench、pitfalls、negative |
| 0909微优化.md / PERF2.md | 极致性能第二轮（两者重复，取更全的 PERF2） | perf/decoding、perf/mt-stream、pitfalls |
| PERF3.md | 极致性能第三轮（分支 miss / 乱链修复） | perf/decoding、negative、pitfalls |
| 多线程优化.md | MT 编解码 + 级别阶梯 | perf/matchers、perf/mt-stream、completed |
| PLAN-mt-stream.md | 流式 MT 方案与数据 | perf/mt-stream、bench/matrix |
| WORK.md | 最新一批（MT prefill / u32 表项 / 流式 MT 等） | perf/\*、pitfalls、todo |
| BENCH-MATRIX.md | 广域 perf 矩阵 | bench/matrix |
| COMPARE.md | 与 libzstd 实现层对比 | status、bench、todo |
