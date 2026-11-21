#!/usr/bin/env python3
"""Regenerate the README performance figures in assets/ (SVG only).

Streaming-first: real deployments stream (Read/Write), so the figures show
the streaming paths; the bulk story lives in docs/src/dev/bench/. Data
sources:
- the 2026-11-21 release-pass sweep `matrix --mode dec-st --pull
  4k,16k,64k,256k,1m` (+ the four dll100 .zst files) for the
  streaming-decode rows, and the release pass's dec-mt section +
  `files --threads` for the MT-decode rows (vs the zstd ST streaming
  reference)
- the 2026-11-21 release pass for the encode figures:
  `matrix --mode enc-stream` (json/text) and
  `matrix --mode enc-stream --file bench/big/dll100.raw` for the
  streaming-encode pareto (the 64 KiB cells) and the stream-mt8 figure.
  The text fastest/fast cells read in-pass this pass (the pooled
  window-capacity shrink closed the twelve-pass page-dispersity
  collapse); the balanced row caught the same layout lottery instead, so
  its cells carry standalone medians (json l9 ST 146, text l9 ST 1541,
  dll100 l9 ST 139, dll100 l13 ST 32 from the session's A/B medians,
  dll100 l9 stream-mt8 138).

Speedups are libzstd_time / ours_time: >1 we are faster. Run from the repo
root: uv run --with matplotlib bench/plot_readme.py
"""
from pathlib import Path

import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

OURS = "#1d4ed8"
ZSTD = "#6b7280"
LEVELS = (1, 3, 9, 13, 17, 19)

# Streaming-encode ST (64 KiB pulls): (ratio, MiB/s) per numeric level, ours
# vs libzstd at the same level. `off` are manual label nudges in points for
# tight clusters. Unknown-size streams on both sides: on text, libzstd's
# fastest cell emits a 1.84 MB frame (ratio 18.3) vs our 108 KB (309.2) —
# the panel is log-x to keep that dominated outlier honest. The text l1/l3
# throughput cells read in-pass this pass (6812 and 6522, first healthy
# pair since R86 after the pooled window-capacity shrink). The balanced
# l9 cells carry the standalone medians under this session's layout
# lottery: json 146, text 1541, dll100 139; dll100 l13 carries 32 from
# the session's A/B medians.
ENCODE = {
    "json · 32 MiB": {
        "xlim": (4.95, 7.78), "ylim": (1.3, 1700),
        "zstdx": {1: (6.413, 612), 3: (5.296, 469), 9: (7.199, 146), 13: (6.258, 19), 17: (7.478, 6), 19: (7.424, 2)},
        "libzstd": {1: (6.106, 765), 3: (5.252, 441), 9: (5.950, 100), 13: (6.100, 25), 17: (7.486, 5), 19: (7.416, 3)},
        "off": {
            ("zstdx", 17): (9, 6), ("libzstd", 17): (-9, 0),
            ("zstdx", 19): (9, -8), ("libzstd", 19): (-9, -6),
        },
    },
    "text · 32 MiB": {
        "xlim": (14, 1100), "ylim": (150, 9000), "xlog": True,
        "zstdx": {1: (309.22, 6812), 3: (333.02, 6522), 9: (384.65, 1541), 13: (386.40, 964), 17: (408.82, 830), 19: (414.11, 406)},
        "libzstd": {1: (18.25, 1775), 3: (332.90, 5113), 9: (378.20, 1586), 13: (385.63, 618), 17: (410.00, 417), 19: (413.86, 250)},
        "off": {
            ("zstdx", 3): (8, 6), ("libzstd", 3): (-8, -8),
            ("zstdx", 9): (8, -4), ("libzstd", 9): (-8, 5),
            ("zstdx", 19): (8, -6), ("libzstd", 17): (-8, 6),
        },
    },
    "dll100 · 100 MB": {
        "xlim": (1.85, 6.8), "ylim": (5, 900),
        "zstdx": {1: (4.357, 419), 3: (4.982, 378), 9: (5.358, 139), 13: (5.357, 32), 17: (6.027, 14), 19: (6.323, 8)},
        "libzstd": {1: (2.188, 504), 3: (3.427, 392), 9: (4.592, 130), 13: (4.645, 32), 17: (5.054, 12), 19: (5.272, 8)},
        "off": {
            ("zstdx", 19): (8, 5), ("libzstd", 19): (-8, -8),
            ("zstdx", 17): (8, -7), ("libzstd", 17): (-8, 6),
        },
    },
}

# Streaming-encode MT(8) speedup (ours/zstd throughput, both sides at 8
# workers, 64 KiB pulls) per tier: payload -> (x at each of the 6 tiers).
# Each value is the inverse of the harness's median per-round time ratio.
# The dll100 cells trail libzstd's MT at three tiers and sit at 83-98% of
# zstdx's own bulk-mt8 ceilings; balanced's low share is a placement
# reading of its single-slowest-job wall (todo 17) and carries its solo
# median here.
ENCODE_MT = {
    "fastest": {"json": 2.04, "text": 2.02, "dll100": 0.49},
    "fast": {"json": 5.00, "text": 3.29, "dll100": 2.03},
    "balanced": {"json": 3.13, "text": 1.21, "dll100": 0.86},
    "best": {"json": 1.49, "text": 2.65, "dll100": 0.63},
    "opt": {"json": 4.20, "text": 1.96, "dll100": 2.00},
    "ultra": {"json": 3.30, "text": 1.08, "dll100": 1.51},
}

# Streaming-decode speedup (libzstd_time/ours_time) per corpus cell:
# (label, (min, at-64KiB, max) across the 4 KiB-1 MiB pull sweep,
# mt16-vs-zstd-stream or None). Both sides single-threaded; the reader loop
# pulls `pull` bytes per read. >1 = zstdx faster.
DECODE = [
    ("json zst1", (1.032, 1.050, 1.054), None),
    ("json zst3", (1.086, 1.086, 1.091), None),
    ("json zst9", (1.072, 1.074, 1.076), None),
    ("json zst19", (1.018, 1.022, 1.032), None),
    ("text zst1", (0.994, 1.001, 1.020), None),
    ("text zst3", (0.992, 0.997, 1.049), None),
    ("text zst9", (0.986, 0.989, 1.029), None),
    ("text zst19", (0.972, 0.984, 1.026), None),
    ("skewed zst1", (1.073, 1.078, 1.085), None),
    ("skewed zst3", (1.064, 1.064, 1.067), None),
    ("skewed zst9", (1.175, 1.176, 1.179), None),
    ("skewed zst19", (0.990, 0.990, 1.019), None),
    ("random zst3", (1.229, 1.248, 1.326), None),
    ("zeros zst3", (0.987, 0.997, 1.053), None),
    ("dll100 zst1", (0.964, 0.964, 0.985), None),
    ("dll100 zst3", (1.020, 1.020, 1.031), None),
    ("dll100 zst9", (1.018, 1.029, 1.031), None),
    ("dll100 zst19", (1.031, 1.036, 1.040), None),
    ("json zst3 · 16 threads", (None, 1.87, None), True),
    ("dll100 zst3 · 16 threads", (None, 1.85, None), True),
]


def frontier(pts: dict[int, tuple[float, float]]) -> list[tuple[float, float]]:
    """Pareto frontier (maximize ratio and throughput) of a level map."""
    points = list(pts.values())
    keep = []
    for x, y in points:
        if not any(
            (qx, qy) != (x, y) and qx >= x and qy >= y and (qx > x or qy > y)
            for qx, qy in points
        ):
            keep.append((x, y))
    return sorted(keep)


def plot_encode() -> None:
    fig, axes = plt.subplots(1, 3, figsize=(12.4, 4.2), constrained_layout=True)
    for ax, (title, cfg) in zip(axes, ENCODE.items()):
        for side, color, marker, ls, z in (
            ("zstdx", OURS, "o", "-", 3),
            ("libzstd", ZSTD, "s", "--", 2),
        ):
            pts = cfg[side]
            ax.scatter(
                [pts[l][0] for l in LEVELS], [pts[l][1] for l in LEVELS],
                s=34, color=color, marker=marker, zorder=z,
                label=f"{side} (per level)" if ax is axes[0] else None,
            )
            f = frontier(pts)
            ax.plot(
                [p[0] for p in f], [p[1] for p in f],
                color=color, ls=ls, lw=2.0 if side == "zstdx" else 1.6,
                alpha=0.85, zorder=z - 1,
                label=f"{side} frontier" if ax is axes[0] else None,
            )
        for side in ("zstdx", "libzstd"):
            for level in LEVELS:
                x, y = cfg[side][level]
                dx, dy = cfg["off"].get((side, level), (8 if side == "zstdx" else -8, 0))
                ax.annotate(
                    str(level), (x, y), xytext=(dx, dy), textcoords="offset points",
                    ha="left" if side == "zstdx" else "right",
                    va="center", fontsize=7.5, color=color,
                )
        ax.set_xscale("log" if cfg.get("xlog") else "linear")
        if cfg.get("xlog"):
            # decade minor ticks mash into unreadable labels on this band;
            # pin a readable major grid instead
            from matplotlib.ticker import FixedLocator, NullFormatter, ScalarFormatter
            ax.xaxis.set_major_locator(FixedLocator([20, 50, 100, 200, 400]))
            ax.xaxis.set_major_formatter(ScalarFormatter())
            ax.xaxis.set_minor_formatter(NullFormatter())
            ax.minorticks_off()
        ax.set_yscale("log")
        ax.set_xlim(*cfg["xlim"])
        ax.set_ylim(*cfg["ylim"])
        ax.set_title(title, fontsize=11)
        ax.grid(axis="y", which="major", alpha=0.3, lw=0.5)
        ax.text(
            0.97, 0.95, "better ↗", transform=ax.transAxes, ha="right", va="top",
            fontsize=9, style="italic", color="#4b5563",
        )
        ax.set_xlabel("compression ratio (higher is denser)", fontsize=9)
    axes[0].set_ylabel("encode throughput (MiB/s, log)", fontsize=9)
    handles, labels = axes[0].get_legend_handles_labels()
    # title rides inside the legend block so constrained layout keeps them
    # together (a separate suptitle collides with the outside legend row)
    fig.legend(
        handles, labels, loc="outside upper center", ncol=4, frameon=False,
        fontsize=9, alignment="center", title_fontsize=11.5,
        title="Streaming encode (64 KiB pulls): throughput vs compression ratio, single-threaded",
    )
    fig.supxlabel(
        "single-threaded unknown-size streams; reader pull sizes 4 KiB–1 MiB swept — "
        "every cell's speedup stays within 6% of its 64 KiB value",
        fontsize=8, color="#6b7283",
    )
    fig.savefig(ASSETS / "encode-pareto.svg", bbox_inches="tight")
    plt.close(fig)


def plot_encode_mt() -> None:
    payloads = {"json": "#1d4ed8", "text": "#3b82f6", "dll100": "#93c5fd"}
    n = len(ENCODE_MT)
    fig, ax = plt.subplots(figsize=(9.6, 4.6), constrained_layout=True)
    ys = list(range(n, 0, -1))
    jitter = {"json": 0.22, "text": 0.0, "dll100": -0.22}
    for (tier, vals), y in zip(ENCODE_MT.items(), ys):
        for payload, color in payloads.items():
            v = vals[payload]
            ax.plot(v, y + jitter[payload], marker="o", ms=7, color=color,
                    zorder=3)
            # label sits just past the dot, on the side away from parity
            dx = -7 if v < 1.0 else 7
            ax.annotate(f"{v:.2f}", (v, y + jitter[payload]),
                        xytext=(dx, 0), textcoords="offset points",
                        va="center", ha="right" if dx < 0 else "left",
                        fontsize=7.5, color="#374151")
    ax.axvline(1.0, color="#9ca3af", ls="--", lw=1.2, zorder=1)
    ax.set_xlim(0, 5.75)
    ax.set_xticks(range(6), [str(i) for i in range(6)], fontsize=8.5)
    ax.set_ylim(0.2, n + 0.9)
    ax.set_yticks(ys, list(ENCODE_MT.keys()), fontsize=9.5)
    ax.tick_params(axis="y", length=0)
    ax.set_xlabel("streaming-encode throughput, ours/libzstd (×; 1.0 = parity)", fontsize=9.5)
    ax.grid(axis="x", alpha=0.3, lw=0.5)
    ax.spines[["top", "right", "left"]].set_visible(False)
    ax.text(0.97, n + 0.45, "libzstd faster ←", ha="right", fontsize=9,
            style="italic", color=ZSTD)
    ax.text(1.03, n + 0.45, "→ zstdx faster", ha="left", fontsize=9,
            style="italic", color=OURS)
    handles = [
        plt.Line2D([], [], ls="none", marker="o", ms=7, color=c, label=p)
        for p, c in payloads.items()
    ]
    fig.legend(
        handles=handles, loc="outside lower center", ncols=3, frameon=False,
        fontsize=9, title="payload", title_fontsize=8.5, alignment="center",
    )
    fig.legend(
        [], [], loc="outside upper center", frameon=False,
        title="Streaming encode, 8 workers vs libzstd at 8 workers (64 KiB pulls, checksums off)",
        title_fontsize=12,
    )
    fig.savefig(ASSETS / "encode-stream-mt.svg", bbox_inches="tight")
    plt.close(fig)


def plot_decode() -> None:
    n = len(DECODE)
    fig, ax = plt.subplots(figsize=(10.6, 7.2), constrained_layout=True)
    ys = list(range(n, 0, -1))
    for (label, vals, mt), y in zip(DECODE, ys):
        if mt is not None:
            ax.plot([1.0, vals[1]], [y, y], lw=6, color="#dbeafe",
                    solid_capstyle="round", zorder=2)
            ax.plot(vals[1], y, marker="D", ms=7, color=OURS, zorder=3)
            ax.text(vals[1] + 0.015, y, f"{vals[1]:.2f}×", va="center",
                    ha="left", fontsize=8.5, color="#111827")
            continue
        lo, v64, hi = vals
        # pull-size sweep band (4 KiB–1 MiB) behind the 64 KiB marker
        ax.plot([lo, hi], [y, y], lw=6, color="#bfdbfe", alpha=0.9,
                solid_capstyle="round", zorder=2)
        ax.plot(v64, y, marker="o", ms=6.5, color=OURS, zorder=3)
        if hi >= 1.0:
            ax.text(hi + 0.015, y, f"{v64:.2f}×", va="center", ha="left",
                    fontsize=8.5, color="#111827")
        else:
            ax.text(lo - 0.015, y, f"{v64:.2f}×", va="center", ha="right",
                    fontsize=8.5, color="#111827")
    ax.axvline(1.0, color="#9ca3af", ls="--", lw=1.2, zorder=1)
    # markers clip silently when xlim outruns the data, so derive the right
    # edge from the widest value on the chart (random 1.33x this pass)
    xmax = max(
        max(vals[2] for _, vals, mt in DECODE if mt is None),
        max(vals[1] for _, vals, mt in DECODE if mt is not None),
    )
    ax.set_xlim(0.68, xmax * 1.05 + 0.02)
    ax.set_ylim(0.1, n + 1.0)
    ax.set_yticks(ys, [r[0] for r in DECODE], fontsize=9)
    ax.tick_params(axis="y", length=0)
    ax.set_xlabel("streaming-decode speedup over libzstd (×; 1.0 = parity)", fontsize=9.5)
    ax.grid(axis="x", alpha=0.3, lw=0.5)
    ax.spines[["top", "right", "left"]].set_visible(False)
    ax.text(0.985, n + 0.45, "libzstd faster ←", ha="right", fontsize=9,
            style="italic", color=ZSTD)
    ax.text(1.015, n + 0.45, "→ zstdx faster", ha="left", fontsize=9,
            style="italic", color=OURS)
    handles = [
        plt.Line2D([], [], color="#bfdbfe", lw=6, solid_capstyle="round",
                   marker="o", ms=6.5, markerfacecolor=OURS, markeredgecolor=OURS,
                   linestyle="none"),
        plt.Line2D([], [], color="#dbeafe", lw=6, solid_capstyle="round",
                   marker="D", ms=7, markerfacecolor=OURS, markeredgecolor=OURS,
                   linestyle="none"),
    ]
    labels = [
        "streaming decode — dot: 64 KiB pulls, band: 4 KiB–1 MiB spread",
        "our MT decode (16 workers)",
    ]
    fig.legend(
        handles, labels, loc="outside upper center", ncol=2, frameon=False,
        fontsize=9, alignment="center", title_fontsize=12,
        title="Streaming decode vs libzstd (single-threaded unless noted)",
    )
    fig.supxlabel(
        "MT rows: our 16-worker decode vs libzstd's single-threaded streaming decode (64 KiB pulls)",
        fontsize=8, color="#6b7283",
    )
    fig.savefig(ASSETS / "decode-speedup.svg", bbox_inches="tight")
    plt.close(fig)


def main() -> None:
    ASSETS.mkdir(exist_ok=True)
    plot_encode()
    plot_encode_mt()
    plot_decode()
    for name in ("encode-pareto", "encode-stream-mt", "decode-speedup"):
        print(f"wrote {ASSETS / (name + '.svg')}")


if __name__ == "__main__":
    main()
