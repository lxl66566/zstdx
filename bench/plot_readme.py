#!/usr/bin/env python3
"""Regenerate the README performance figures in assets/ (SVG only).

Data source: the 2026-09-19 release bench pass —
- docs/src/dev/bench/matrix.md T1 (decode ST) and T3 (encode ST bulk)
- `zstdx-bench files --threads 2,4,8,16` for the MT-decode bars
(json.zst3 mt16 / dll100.zst3 mt16 vs the zstd ST streaming reference)

Run from the repo root: uv run --with matplotlib bench/plot_readme.py
"""
from pathlib import Path

import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

OURS = "#1d4ed8"
ZSTD = "#6b7280"
LEVELS = (1, 3, 9, 13, 17, 19)

# Encode ST bulk: (ratio, MiB/s) per numeric level, ours vs libzstd at the
# same level. `off` are manual label nudges in points for tight clusters.
ENCODE = {
    "json · 32 MiB": {
        "xlim": (4.95, 7.78), "ylim": (2.2, 1700),
        "zstdx": {1: (6.39, 600), 3: (5.30, 458), 9: (7.20, 142), 13: (6.26, 27), 17: (7.49, 6), 19: (7.42, 3)},
        "libzstd": {1: (6.11, 858), 3: (5.29, 477), 9: (5.95, 122), 13: (6.10, 37), 17: (7.49, 7), 19: (7.42, 3)},
        "off": {
            ("zstdx", 3): (8, 5), ("libzstd", 3): (-8, -8),
            ("zstdx", 13): (8, 4), ("libzstd", 13): (-8, -6),
            ("zstdx", 17): (9, 0), ("zstdx", 19): (9, -11),
            ("libzstd", 17): (-10, 7), ("libzstd", 19): (-10, -10),
        },
    },
    "text · 32 MiB": {
        "xlim": (305, 419), "ylim": (230, 24000),
        "zstdx": {1: (309.22, 14212), 3: (333.02, 14085), 9: (384.64, 1178), 13: (386.39, 824), 17: (410.18, 773), 19: (414.04, 393)},
        "libzstd": {1: (308.94, 10455), 3: (332.90, 7275), 9: (378.41, 1734), 13: (385.90, 742), 17: (410.11, 463), 19: (413.98, 272)},
        "off": {},
    },
    "dll100 · 100 MB": {
        "xlim": (1.85, 6.3), "ylim": (5.5, 900),
        "zstdx": {1: (2.15, 392), 3: (3.47, 411), 9: (5.05, 101), 13: (5.35, 28), 17: (5.64, 13), 19: (5.89, 8)},
        "libzstd": {1: (2.19, 530), 3: (3.40, 434), 9: (4.62, 151), 13: (4.67, 39), 17: (5.06, 14), 19: (5.28, 9)},
        "off": {("zstdx", 3): (8, 4), ("libzstd", 3): (-8, -7)},
    },
}

# Decode ST bulk speedup (zstd MiB/s ÷ zstdx MiB/s) per corpus cell.
DECODE = [
    ("json", [("zst1", 1.36), ("zst3", 1.26), ("zst9", 1.40), ("zst19", 1.68)]),
    ("text", [("zst1", 2.64), ("zst3", 3.48), ("zst9", 3.68), ("zst19", 3.72)]),
    ("skewed", [("zst1", 1.50), ("zst3", 1.17), ("zst9", 0.98), ("zst19", 1.48)]),
    ("random", [("zst3", 3.80)]),
    ("zeros", [("zst3", 4.73)]),
    ("dll100", [("zst1", 1.12), ("zst3", 1.12), ("zst9", 1.22), ("zst19", 1.17)]),
]
# MT decode (16 workers); libzstd has no MT decode — reference is its
# single-threaded streaming decoder.
DECODE_MT = [("json zst3 · 16 threads", 1.17), ("dll100 zst3 · 16 threads", 1.30)]


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
        ax.set_xscale("linear")
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
    fig.legend(handles, labels, loc="outside upper center", ncol=4, frameon=False, fontsize=9)
    fig.savefig(ASSETS / "encode-pareto.svg", bbox_inches="tight")
    plt.close(fig)


def plot_decode() -> None:
    rows: list[tuple[str, float, bool]] = []
    for shape, cells in DECODE:
        for tag, speedup in cells:
            rows.append((f"{shape} {tag}", speedup, False))
    for label, speedup in DECODE_MT:
        rows.append((label, speedup, True))

    fig, ax = plt.subplots(figsize=(9.0, 6.4), constrained_layout=True)
    n = len(rows)
    ys = list(range(n, 0, -1))
    for (label, speedup, mt), y in zip(rows, ys):
        color = "#93c5fd" if mt else OURS
        ax.barh(y, speedup, height=0.62, color=color, edgecolor=OURS if mt else "none",
                hatch="//" if mt else None, zorder=3)
        ax.text(speedup + 0.06, y, f"{speedup:.2f}×", va="center", fontsize=8.5, color="#111827")
        ax.text(-0.12, y, label, ha="right", va="center", fontsize=8.5,
                color="#374151", transform=ax.get_yaxis_transform())
    ax.axvline(1.0, color="#9ca3af", ls="--", lw=1.1, zorder=2)
    ax.text(1.0, n + 0.75, "libzstd parity", ha="center", fontsize=8.5, color="#6b7280")
    ax.set_yticks([])
    ax.set_xlim(0, 5.4)
    ax.set_ylim(0.1, n + 1.1)
    ax.set_xlabel("decode speedup over libzstd (×, higher is better)", fontsize=10)
    ax.grid(axis="x", alpha=0.3, lw=0.5)
    ax.spines[["top", "right", "left"]].set_visible(False)
    ax.text(
        0.0, -0.115,
        "hatched: our multi-threaded decode (16 workers); libzstd has no MT decode, "
        "reference = its single-threaded streaming decoder.\n"
        "bulk single-threaded elsewhere; streaming decode is the one column where "
        "libzstd stays ahead on compressible shapes (see docs).",
        transform=ax.transAxes, fontsize=7.8, color="#6b7283", va="top",
    )
    fig.savefig(ASSETS / "decode-speedup.svg", bbox_inches="tight")
    plt.close(fig)


def main() -> None:
    ASSETS.mkdir(exist_ok=True)
    plot_encode()
    plot_decode()
    print(f"wrote {ASSETS / 'encode-pareto.svg'}")
    print(f"wrote {ASSETS / 'decode-speedup.svg'}")


if __name__ == "__main__":
    main()
