# Dictionary trainer
## Sorted concatenation poisons selection


The fastCover epochs partition the concatenated training body. With a sorted body, ordered alphabetically by glob, every epoch is a cluster of near-identical files, each epoch's best segment describes its cluster instead of the collection, and the positional 75/25 train and test split lands on a distribution shift. On the systemd fixture this alone was worth about 20% dictionary quality, with holdout 7,371 to 6,030 B at matched k against a libzstd -9 referee.

The fix is a fixed-seed Fisher-Yates shuffle over sample order before concatenation, a port of libzstd's `DiB_shuffle`, which is deterministic and representative. A related trap is that `zstd --train-fastcover=k=K,d=D` with forced parameters, which is the non-optimizer path, trains on all samples, because the 75/25 split exists only in the optimizer. Forced-k dictionaries therefore leak the test set in A/B comparisons, and a leaked k=537 dictionary scored 5,685 against the unleaked 6,085.
## Trainer test noise must be aperiodic


The `(i*131 + j*17) as u8` noise repeats its 8-grams about 30 times across files, because all files land on the same 256 k-mers under mod-256 arithmetic. A 61-byte pattern repeated 8 times also lets a shared or noise-straddling window cover all distinct shared k-mers plus free noise coverage. Both made the noise out-score the shared block. LCG noise with per-file seeds and a non-repetitive shared block keep the assertion meaningful.
## Entropy preloading versus heuristics


The seeded tables must compete on measured costs, and the tuned heuristics starve them three ways, measured on the systemd holdout through cross-fed dictionary decomposition. The Huffman treeless gate `diff > 5`, a sum of code-length differences, rejects dictionary tables that would win by tens of bytes, and the right comparison is stream bits against stream bits plus the exact serialized description in the `HUF_estimateCompressedSize` form. The FSE predefined thresholds, with `nb_seq < 72` selecting predefined, fire before the repeat check, so dictionary tables never reach the small blocks where their statistics matter, while libzstd's lazy and above paths compare predefined, repeat and fresh purely on costs. Treeless blocks below 1 KiB should take the single-stream literals form, with no 6-byte jump table and one padding tail instead of four, which libzstd emits whenever `repeat_valid && lhSize == 3`.

Landing all three took zstdx -9 with our formatted dictionary from +11.6% to +3.0% against libzstd on the holdout, and flipped the formatted-versus-raw delta from +3.1%, worse than raw, to -5.7%. All of it is gated on a per-stream "still dictionary statistics" flag, and every no-dict path keeps the stock selection with a byte-identical full-ladder dump.
## Cross-fed dictionary decomposition


Train both trainers on the same corpus, then run {our encoder, libzstd} against {our raw content, libzstd raw content, libzstd formatted} over the holdout. Same-encoder raw-content pairs isolate trainer quality, same-dictionary encoder pairs isolate the encoder, and formatted-versus-raw per encoder isolates the entropy-preload handling.

On this fixture the split showed trainer parity, an encoder-side +7.8% small-band residue, and an entropy component that was actively harmful, which is three different levers from one measurement.
## Dictionary fill follows load semantics


`load_dictionary` had routed through `prefill_window`, inheriting the strip's stride-3 grid and Fast's retention-tail cap, which exist because a window-long strip cannot afford a dense fill. A dictionary is at most one window and the payload parses mostly against it, so coverage dominates over fill cost. Libzstd fills chain rows densely through `ZSTD_insertAndFindFirstIndex`, and the CLI's small files go through CDict attach, whose tables carry the `dtlm_full` backfill. Reusing a strip-tuned prefill for another consumer of the same tables silently dropped two thirds of the dictionary's candidates for chain or the whole dictionary head for Fast's cap.

The level fingerprint was a chain-row-only end-to-end gap, worst at -9, with -3 and dfast at parity because both sides stride-3 there. A corollary is that the dictionary chain path needs no `acquire_seed` once dense, measured byte-identical, because the seed's period-repeat compensation is for single-candidate head tables and dense links already reach every position.

A second knob in the same class is that our `search_depth` table halves libzstd's `1 << searchLog` attempt counts as a big-payload speed tuning. Parity holds there, but a dictionary parse walks a long dense chain where the full budget converts, worth +2.1% to +1.9% raw control at -9 on top of the fill fix.
## Filter seeding is phase-critical


Seeding the incompressibility gate's repeat probe from a job strip with one whole-strip stride hits a modular lottery. A period-P repeat probes a hit only for blocks whose sample grid happens to align with the seed stride modulo P/gcd. On a synthetic 5 MiB-period corpus, four of five jobs lost the far class while the one job whose strip length picked a coprime stride kept it: a 12 MiB strip derived stride 385, which is 5*7*11 and shares factors with the block stride 33, while an 8 MiB strip gave 257, a prime, which was lucky.

The fix replays the consumer itself, with the same block-sized segments, the same per-segment stride and the same phase restart, so a repeat at a block-multiple displacement lands on the segment grid a scan would have planted. A future "pre-plant a sampled filter from history" must copy the consumer's exact sampling geometry, and matching its density is not enough.
