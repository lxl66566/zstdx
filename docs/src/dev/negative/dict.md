# Falsified directions — dictionary trainer

> Fixture: systemd pool (140 files, 512 B-4 KiB), seeded splits into 86 train / 54 holdout, 16 KiB dicts, referee = per-file frames through the zstdx encoder summed over the holdout. "CLI" = `zstd --train` v1.5.7 defaults (fastCover optimizer, steps=4, split 0.75, f=20, d=8).

| tried | result | why it fails / what to remember |
|---|---|---|
| scoring the k sweep at a different level (3/5/9/12) or on the finalized dict instead of raw content (C's optimizer scores `ZDICT_finalizeDictionary` output at level 3) | argmin-k IDENTICAL at every level and both forms, all 3 seeds | the internal 22-file metric's k ranking is level- and form-invariant; C's formatted metric is not the source of its behavior — the candidate GRID is. Do not re-try metric variants to move the pick |
| the compact 9-value k ladder ({200..2000}, 4 near-neighbors in the 400-800 band) | raw regret vs the dense-grid oracle +854/+124/+643 B per seed; the CLI's 5-value ladder (50/537/1024/1511/2000) lands +0/+124/+224 | winner's curse: the argmin over more, closer candidates chases the internal metric's noise harder; a coarse well-separated ladder picks better despite seeing fewer options |
| libzstd's f=20 bucket hash as the frequency-table key (exact match to C) | content at MATCHED k was 73-154 B BETTER on 2/3 seeds (collisions act as approximate-match regularization), but the metric's picks got worse: net +1954 B over 3 seeds vs exact hashing at the CLI ladder | the collision regularization and the pick quality are separable effects; a bucket-keyed table with an exact-keyed metric (or a smoothed exact key) is the unexplored middle — revisit if the pick lottery is ever tamed |
