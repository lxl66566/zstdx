# Falsified directions: dictionary trainer

The fixture is a systemd pool of 140 files from 512 B to 4 KiB, with seeded splits into 86 train and 54 holdout files, 16 KiB dictionaries, and a referee that sums per-file frames through the zstdx encoder over the holdout. The CLI column means `zstd --train` v1.5.7 defaults: the fastCover optimizer with steps=4, split 0.75, f=20 and d=8.

## K selection

### Scoring the k sweep elsewhere

The idea was to score the k sweep at a different level, 3, 5, 9 or 12, or on the finalized dict instead of raw content. C's optimizer scores `ZDICT_finalizeDictionary` output at level 3. The argmin-k was identical at every level and in both forms across all 3 seeds. The internal 22-file metric's k ranking is level- and form-invariant, and C's formatted metric is not the source of its behavior. The candidate grid is. Do not retry metric variants to move the pick.

### Compact k ladder

The idea was a compact 9-value k ladder from 200 to 2000, with 4 near-neighbors in the 400-800 band. Raw regret against the dense-grid oracle read +854/+124/+643 B per seed, while the CLI's 5-value ladder of 50/537/1024/1511/2000 landed +0/+124/+224. This is the winner's curse: the argmin over more and closer candidates chases the internal metric's noise harder. A coarse, well-separated ladder picks better despite seeing fewer options.

### Bucket hash as the frequency key

The idea was libzstd's f=20 bucket hash as the frequency-table key, an exact match to C. At the matched k the content was 73-154 B better on 2 of 3 seeds, because collisions act as approximate-match regularization, but the metric's picks got worse: +1954 B net over 3 seeds against exact hashing at the CLI ladder. Collision regularization and pick quality are separable effects. A bucket-keyed table with an exact-keyed metric, or a smoothed exact key, is the unexplored middle. Revisit it if the pick lottery is ever tamed.

## Table construction and statistics

### C's construction over our histograms

The idea was C's table construction over our histograms, splicing the header by linking libzstd's `HUF_buildCTable_wksp`, `FSE_normalizeCount` and `FSE_writeNCount` over our dumped counts with the same content. The result was +22 to +94 B per referee cell worse than our in-tree normalize and serialize on the same histograms. The finalize gap was never construction, because ours is ahead of C's at equal histograms. All of it was the stats-parse histograms, closed by the stats level and sample-set change recorded in [encoding](../perf/encoding.md).

### Stats-parse level 3

The idea was stats-parse level 3, C's `ZSTD_CLEVEL_DEFAULT` pin. Our level-3 row is the worst stats source on every seed: its dfast parse is about 30% denser than libzstd's at the same level, at 25.3 against 19.4 sequences per sample, and its tables cost +140-190 B per seed set against the level 4-12 plateau. C's default encodes libzstd's level-3 parse shape and not ours. The stats level is an engine-relative knob, where level 4 landed and the 4-12 plateau is flat within about 40 B.

### Stats from the training share only

The idea was to take stats from the scoring split's training share only, mirroring C's `nbFinalizeSamples` at 100% of the train split. Using every input sample for the entropy stats was 8-142 B better per cell at every stats level. The 75/25 split exists to score the k sweep, and every sample is equally valid distribution evidence for the tables. The narrower set is less data.

### Constant K and sample shaping

The idea was `K_DEFAULT` at 1024 plus per-sample input shaping for the stats parse. The stats were byte-identical on this fixture, because dict content dominates the shape clamp. The C-shaped average-sample form was kept as the principled variant. Shape effects reappear only when sample sizes vary by orders of magnitude, and they are not measurable at 0.5 to 4 KiB.

## Allocation and layout

### Alloc-churn layout tax on dict paths

The audit asked whether the same-process alloc-churn layout tax reaches the dictionary paths. The trainer is `train_formatted` on 1024 x 8 KiB json samples, producing an 8 MiB body with a 2.1M-key k-mer table and a 64 KiB dict at about 2.07 s per call. Consumers were timed on 64 holdout samples as pooled dict decode through `FrameDecoder` and `add_dict` on the ring path, and as the one-shot `compress_with` and `decompress_with` dict APIs. The churn was the encoder L13 to L3 pooled transition or a prior full train pass in the same process.

The trainer read 2.089 s solo against 2.065 s after encoder churn, which is -1.1% and inside noise, and 4 sequential in-process train calls were flat. Pooled dict decode read 860/862/862 MiB/s for solo, encoder churn and train churn. One-shot dict decode read 411/410/402 MiB/s, with the worst at -2.2% and inside the band. One-shot dict encode took 0.81/0.78/0.79 s per 512 KiB batch, with churned runs 3-4% faster.

There is no tax anywhere. The trainer's big allocations, the counts HashMap and the shuffled body, live exactly one call, and both one-shot dict APIs re-allocate per call, with `EncDictionary::parse` per `compress_with` and a fresh `FrameDecoder` per `decompress_with`, so the frozen-layout mechanism cannot attach. The pooled dict decoder's hot set of 64 KiB dict content, a small window and the entropy tables is cache-scale. Dict paths need no hugepage wiring even once the encoder-side wrapper lands.

## Chain walk width

### Disarming the x8 walk with the width

The idea was to disarm the x8 chain walk budget together with the width at 32 MiB dict-ST and a 110 KiB formatted dict. json -9 moved from 4,589,941 to 4,697,443, or +2.3%, and text -9 from 59,827 to 61,797, or +3.3%. The depth converts once the width is stock. The disarm's one wall win was json -9 dict-ST at 86 against 13 MiB/s. The dilution was the width-4 and mml-4 pair alone. The deep walk is ratio-positive at every dict span, and its walk cost sits in the ST dict cell's documented 13-17 MiB/s class, with dict-MT4 as the speed path at 117 MiB/s and x9.0.

### Width 4 armed at every span

The idea was to keep width 4 armed below or at every span, the residual of the landed form. text -9 dict-ST read 59,827 against the armed build's 59,082, or +1.3%, and still -0.4% against libzstd. Width 4 pays on text's 4-byte word vocabulary at any span, while json needs it off above 256 KiB at -14 to -16%. A payload-shape key, with width 4 only when the head looks word-tiled, would spend a head screen on a non-hot path to serve one cell. It is recorded and not taken.

## Reach probe on dictionary MT

### Arming the reach probe

Arming the reach probe on dictionary MT frames is falsified by construction, without a measurement. SHRINK's verdict is W12 at 4 KiB, while dict history sits at distances up to dict_len, 110 KiB in the standard case, so every dict-distance match turns illegal frame-wide. The dict's seed, declared through dictID and paid for in job zero's reset, is dead weight under the only non-Keep outcome. That history needs the reach it has. The probe's shrink arm destroys the dictionary, and a faithful probe would also need both sides dict-seeded, because the keep donation rides borrowed-window pooled states that cannot host a dict prefix. Dictionary frames keep the forced Keep on every MT path, ST dict entries never probe either, and the capture classes arm without it, as recorded in [mt-stream](../perf/mt-stream.md).
