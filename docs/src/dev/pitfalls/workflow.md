# 踩坑记录 · 工程与基准方法论

## 噪声与判据

- 本机（Zen4 32C）噪声 ±10% 且有**慢漂移相位**：上午稳定 ±1%，下午同一二进制可慢
  25-50%。跨时段结论只认 git stash 背靠背同机 A/B；单次 bench 必须二次复跑。
- random 双峰（1388↔2027 MiB/s）是频率漂移（lscpu scaling 73%）+ SMT 兄弟核调度；
  skewed.Fastest 读数受兄弟核影响大，复测一轮即回位。
- **确定性输出 sizes 是唯一免费可信的 A/B 信号**（对不该改输出的改动）；速度结论
  需多轮取中位或进程内开关，单轮 ±10% 内的"收益"不可信。bench_matrix 的 x 因子
  跨 run 漂 ±10%（未改动格子也漂）。
- bench 位置偏差：同一测试中后跑的二进制稳定占 4-5% 便宜（缓存/频率相位）——A/B
  必须位置对齐（交替先跑/后跑，按位置比较）。
- **分配器交叉污染**：同进程串跑多实现/多尺寸时，zstd 侧 CCtx 分配 churn 打碎
  malloc arena → 后续计时段 3 倍失真（random-64K 串跑 ~3000 vs 单形状进程 9200）。
  小负载判据必须 IMPL+SIZE 钉死单形状单尺寸进程。
- glibc 动态 mmap/trim 阈值随分配序列漂移：固定 `MALLOC_TRIM_THRESHOLD_` 反而废掉
  zstd 的自适应（其 random-64K 崩 3 倍）；小语料要么固定要么隔离运行。
- 构建扰动后首跑读数可假性偏低 30%；终态数字取多轮稳定值。
- solo bench 的进程内格子顺序是分配布局伪影源（表格尺寸变化移动后续分配落位）；
  A/B 定夺用新鲜进程，或至少两种环境交叉验证。

## cargo / git

- **A/B 一律 `git stash` 背靠背同机执行**；stash 往返后 `target/` 不自动重建——
  曾对着 BASE 旧二进制 objdump 分析半天"bmi2 没生成"。
- `git stash push --staged` 在本仓 git 版本上只复制不还原现场；分原子提交用
  `git restore --staged` + `git commit <path>`。
- **cargo fmt 会波及历史未格式化文件**：提交前 `git checkout --` 剔除无关文件
  （差点把 6 个无关文件带进原子提交）。
- `cargo build --no-default-features` 有缓存假象：必须 touch 源码确认重编译再数
  警告；且它会把 example 重编译为 no-std 并**覆盖 target/release 下的默认构建**
  （AVX-512 内核被 cfg 掉、xxhash 消失），之后的测量全部失真——测量前重建默认
  构建并用 `nm ... | grep <内核符号>` 验证。no-default 检查须带 `-p zstdx`
  （根目录构建时 cli 会把 hash feature 带回）。（examples 已移出 zstdx，此风险
  消除。）
- **`cargo test` 的管道退出码是 tail 的**：`cargo test | tail` 时 cargo 失败被
  `&&` 链吞掉、成功标记照打。测试门必须 `cargo test > log; echo $?` 落盘查退出码
  与 FAILED 计数；`2>&1 | tail -N` 也会把输出文件截成最后 N 行（重跑浪费一轮）。
- **debug 测试套件是必跑门**（release 全绿 ≠ 无 bug）；套件疑似卡死先怀疑"某个
  panic 毒化了线程池"（MT worker panic → join 永等，17 分钟零输出），用
  `--test-threads 1` + 直接跑测试二进制定位。
- 直接运行测试二进制要在 crate 根目录（13 个语料测试用相对路径）。
- bench corpus 不在 git（untracked 生成物）：worktree 跑 bench 先拷贝。
- 调试 panic / eprintln 插桩提交前必须删（曾遗留一处；`#[cfg(feature="std")]` 的
  eprintln 会随默认 feature 编进 release）。

## perf / 归因

- `setarch -R perf record`（关 ASLR）+ `perf script` 提取 IP 直方图 + objdump
  对应地址段人工读；**`perf annotate --stdio` 输出混入内核反汇编不可信**。
- **annotate 窗口开太大（+0x1000）会把相邻符号算进来**（FSETable::clone 曾被误当
  emit_match 内部指令）；先 `nm` 拿准确地址再用小窗口。
- AMD 分支 miss 事件采样 skid：stub 二分 + perf stat 总量归属（详见解码踩坑页）。
- **指令数判据**：优化后期单行改动的 ±5-25% 墙钟摆动常为布局/调度噪声；
  `perf stat` 指令数下降才是真收益，指令持平 + 墙钟摆动视为无效扰动。micro 改动
  用指令数，端到端改动用交错墙钟中位。
- LD_PRELOAD 计数 memcpy/memmove：定位小拷贝灾难（json 32MiB ~3M 次 PLT 调用）
  的利器。
- **寄存器压力定律**：≥15 活跃值 LLVM 将位窗口按栈驻留，12 值可驻留（annotate 上
  `shlx` 带内存操作数即中招）；单移除内存操作数几乎无效（+0.2%，乱序引擎已隐藏
  访存延迟）、缩短依赖链仅 +1%、**砍串行依赖（成组批提取）收益最大**。

## 工具链 / 环境

- WSL：`stat /mnt/c/...` 随机 I/O error（statx 抽风，cargo/git 在 /mnt/c 全灭）→
  文件内容用 cat/管道；**WSL 读 /mnt/c 大文件可能挂死整个 VM**（wsl --shutdown
  恢复）；分析环境固定在原生 ext4；WSL 空闲自动关闭会清 /tmp（产物放 ~）；
  `wsl -e` 传空参数报错、嵌套引号会坏——复杂命令写成 .sh 用 `wsl bash <path>`。
- Windows 侧无 grep/sed/head -c（用 rg/powershell）；`find` 是 Windows find；
  cmd 的 `;` 不是命令分隔符；powershell 批量 -replace 注意 `` `n `` 转义与正则
  误伤（曾把 as_ref 一并替换）。
- python 解析 zstd 帧三连坑：`(b1<<8)|b2+0x7f00` 的 `+` 优先于 `|`；literals 头
  尺寸段位宽记错；`sort -rn` 混入内核符号。**分析工具先对照已知正确解码器的字段
  定义再写。**
- tar 解包覆盖要用默认（会覆盖）；`--skip-old-files` 是跳过已存在文件——曾导致
  WSL 侧一直跑旧代码。
- 后台任务的输出：管道里避免 tail/head，落盘完整输出再 grep。

## 语料与可复现性

- **语料不可比**：gen_corpus.sh 的 text.raw 平铺 src 树——src 一变语料就变；
  历史上 text 形状两次相变（比率 299.6 / 239 的跨 tile 匹配解锁）都依赖重复周期
  恰落在窗口区间，src 膨胀超过 768K 时会再次塌陷。**考虑固定一个 tar 快照。**
- 跨会话绝对值漂移：l6 json 本会话 171-179 vs 上轮记录 177——回滚 match_generator
  到上一版对照读数相同，确认非回归。**横向对比只信同会话 A/B。**

## 编辑工具

- Edit 工具按文本匹配：X1/X2 两函数尾部上下文相同导致改错函数——改动相似函数时
  old_string 必须带各自独有的标识。
- match 臂编辑：old/new 只含目标臂（曾把下一臂整个复制进 new_string 产生重复臂，
  clippy unreachable pattern 才暴露——行为恰好正确但多余）。
- 批量恢复代码后必须 `git diff` 核对再 bench：python replace 漏一个位点
  （`replace(pat, 2)` 只命中前两个），json.fastest 尺寸漂 -101B，靠确定性尺寸
  对比才发现——**改完先 diff，尺寸确定性是免费回归探针**。
- sweep 脚本：正则替换丢一个逗号致子模式永不匹配——一次"没变"跑出"变了"的结论
  被另一配置数据掩盖。**参数 sweep 必须 print 回读确认**（set_lvl.py 会回显+断言）。
- bench 工具自坑：mibs 公式忘乘轮数（`raw.len()/elapsed` ≠ `raw.len()*n/elapsed`）；
  过滤参数子串不匹配静默跑空进程（utime≈0 是信号）；x 因子 `%.3f` 在极端值处
  不可读（看 MiB/s 列）；zeros 是免费噪声标定格。
- **全局 `core.fsmonitor=true` 会吞掉工具写入的文件变更**：python 脚本/cargo fmt
  改文件后 fsmonitor 守护进程未报告 → git 认为文件"未变更"（blob 实际已不同）→
  status 不显示 dirty、pathspec commit 静默提交旧内容（曾在 fix 提交里丢掉全部
  源码改动，`git show --stat` 文件数对不上才暴露）。**本仓库 git 命令一律
  `git -c core.fsmonitor=false`，提交后必查 stat 文件数**。连带：pathspec commit
  会把索引重置为 HEAD+已提交路径，之前 staged 的 `git mv` 改名整体掉出索引；
  被 .gitignore 忽略但历史上强制跟踪的文件（fuzz artifacts）重暂存时必须
  `git add -f`，否则改名会被提交成纯删除。
