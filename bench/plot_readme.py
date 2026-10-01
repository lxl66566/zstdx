#!/usr/bin/env python3
"""Regenerate the README performance figures in assets/ (SVG only).

Streaming-first: real deployments stream (Read/Write), so the figures show
the streaming paths; the bulk story lives in docs/src/dev/bench/. Data
source: the 2026-10-01 pre-release bench pass at `1249735b` —
- `matrix --mode dec-st --pull 4k,16k,64k,256k,1m` (+ the four dll100 .zst
  files) for the streaming-decode pull sweep, `files --threads 16` for the
  MT-decode rows (vs the zstd ST streaming reference)
- `matrix --mode enc-stream --pull 4k,16k,64k,256k,1m --file
  bench/big/dll100.raw` for the streaming-encode pareto (the 64 KiB cells;
  the sweep itself feeds the figures' pull-size notes), plus a follow-up
  `matrix --mode enc-stream --file bench/big/dll100.raw` run (2026-10-02)
  for the MT figure — T5b takes the same payloads and that run first
  measured the dll100 stream-mt8 cells

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
# the panel is log-x to keep that dominated outlier honest.
ENCODE = {
    "json · 32 MiB": {
        "xlim": (4.95, 7.78), "ylim": (1.8, 1700),
        "zstdx": {1: (6.394, 581), 3: (5.296, 472), 9: (7.128, 85), 13: (6.260, 29), 17: (7.487, 7), 19: (7.425, 3)},
        "libzstd": {1: (6.106, 776), 3: (5.252, 442), 9: (5.950, 104), 13: (6.100, 36), 17: (7.486, 7), 19: (7.416, 3)},
        "off": {
            ("zstdx", 17): (9, 6), ("libzstd", 17): (-9, -7),
            ("zstdx", 19): (9, -8), ("libzstd", 19): (-9, 7),
        },
    },
    "text · 32 MiB": {
        "xlim": (14, 1100), "ylim": (150, 9000), "xlog": True,
        "zstdx": {1: (309.22, 5615), 3: (333.02, 5500), 9: (384.66, 1423), 13: (386.42, 987), 17: (410.22, 744), 19: (414.11, 387)},
        "libzstd": {1: (18.25, 1734), 3: (332.90, 4993), 9: (378.29, 1617), 13: (385.64, 674), 17: (409.99, 424), 19: (413.85, 260)},
        "off": {
            ("zstdx", 3): (8, 6), ("libzstd", 3): (-8, -8),
            ("zstdx", 9): (8, -2), ("libzstd", 9): (-8, 3),
        },
    },
    "dll100 · 100 MB": {
        "xlim": (1.85, 6.8), "ylim": (5, 900),
        "zstdx": {1: (4.346, 516), 3: (4.970, 437), 9: (5.312, 120), 13: (5.353, 28), 17: (6.040, 13), 19: (6.322, 8)},
        "libzstd": {1: (2.188, 509), 3: (3.427, 394), 9: (4.592, 138), 13: (4.645, 37), 17: (5.054, 13), 19: (5.272, 9)},
        "off": {
            ("zstdx", 19): (8, 5), ("libzstd", 19): (-8, -8),
            ("zstdx", 17): (8, -7), ("libzstd", 17): (-8, 6),
        },
    },
}

# Streaming-encode MT(8) speedup (ours/zstd throughput, both sides at 8
# workers, 64 KiB pulls) per tier: payload -> (x at each of the 6 tiers).
# The dll100 cells are the first measured (the MT section only recently
# takes raw --file payloads) — they trail libzstd's MT at four tiers and
# sit at 46-93% of zstdx's own bulk-mt8 ceilings.
ENCODE_MT = {
    "fastest": {"json": 1.65, "text": 1.76, "dll100": 0.28},
    "fast": {"json": 5.68, "text": 3.39, "dll100": 1.23},
    "balanced": {"json": 3.27, "text": 1.05, "dll100": 0.87},
    "best": {"json": 1.64, "text": 2.49, "dll100": 0.07},
    "opt": {"json": 2.46, "text": 1.88, "dll100": 1.04},
    "ultra": {"json": 2.10, "text": 1.56, "dll100": 0.82},
}

# Streaming-decode speedup (libzstd_time/ours_time) per corpus cell:
# (label, (min, at-64KiB, max) across the 4 KiB-1 MiB pull sweep,
# mt16-vs-zstd-stream or None). Both sides single-threaded; the reader loop
# pulls `pull` bytes per read. >1 = zstdx faster.
DECODE = [
    ("json zst1", (0.797, 0.802, 0.802), None),
    ("json zst3", (0.741, 0.743, 0.749), None),
    ("json zst9", (0.773, 0.774, 0.776), None),
    ("json zst19", (0.847, 0.850, 0.851), None),
    ("text zst1", (0.817, 0.823, 0.831), None),
    ("text zst3", (0.929, 0.934, 0.950), None),
    ("text zst9", (0.948, 0.958, 0.967), None),
    ("text zst19", (0.941, 0.952, 0.960), None),
    ("skewed zst1", (0.903, 0.907, 0.915), None),
    ("skewed zst3", (0.786, 0.826, 0.826), None),
    ("skewed zst9", (0.763, 0.763, 0.767), None),
    ("skewed zst19", (0.900, 0.907, 0.907), None),
    ("random zst3", (1.221, 1.234, 1.290), None),
    ("zeros zst3", (0.978, 0.997, 1.018), None),
    ("dll100 zst1", (0.768, 0.774, 0.775), None),
    ("dll100 zst3", (0.736, 0.740, 0.740), None),
    ("dll100 zst9", (0.750, 0.750, 0.752), None),
    ("dll100 zst19", (0.750, 0.750, 0.754), None),
    ("json zst3 · 16 threads", (None, 1.18, None), True),
    ("dll100 zst3 · 16 threads", (None, 1.31, None), True),
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
        "every cell's speedup stays within 5% of its 64 KiB value",
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
            # label on the side away from the parity line; very small dots
            # label to their right so the text stays inside the axis
            if v < 0.5:
                x, ha = v * 1.3, "left"
            elif v < 1.0:
                x, ha = v * 0.82, "right"
            else:
                x, ha = v * 1.2, "left"
            ax.text(x, y + jitter[payload], f"{v:.2f}", va="center",
                    ha=ha, fontsize=7.5, color="#374151")
    ax.axvline(1.0, color="#9ca3af", ls="--", lw=1.2, zorder=1)
    ax.set_xscale("log")
    ax.set_xlim(0.045, 9)
    ax.set_xticks([0.05, 0.1, 0.25, 0.5, 1, 2, 4, 8],
                  ["0.05", "0.1", "0.25", "0.5", "1", "2", "4", "8"], fontsize=8.5)
    ax.set_ylim(0.2, n + 0.9)
    ax.set_yticks(ys, list(ENCODE_MT.keys()), fontsize=9.5)
    ax.tick_params(axis="y", length=0)
    ax.set_xlabel("streaming-encode throughput, ours/libzstd (×; 1.0 = parity, log)", fontsize=9.5)
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
    ax.set_xlim(0.68, 1.42)
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
