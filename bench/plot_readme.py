#!/usr/bin/env python3
"""Regenerate the README performance figures in assets/ (SVG only).

Real-corpus only: every figure cell comes from a real-world payload, so
the synthetic json/text corpus shapes stay in the recorded ratio sweep
for regression comparability but no longer feed a README figure.

Data sources, all raw `matrix` logs tracked under bench/data/:
- `pass-080/dec-st-real.log` (`matrix --mode dec-st --pull
  4k,16k,64k,256k,1m` over jsonreal/textreal plus the four dll100 .zst
  files) for the streaming-decode rows, `pass-080/dec-mt-real.log` and
  `pass-080/files-dll.log` for the MT-decode rows (against the zstd ST
  streaming reference)
- `stream-ladder-20261126/stream-ladder.log` (`matrix --mode enc-stream
  --full-ladder --no-mt --parallel 1 --shape json,text,jsonreal,textreal
  --file bench/big/dll100.raw`, the `stream-ladder` section of
  bench/pass.sh) for the encode pareto, read cell by cell: levels 1-22 on
  both sides at 64 KiB pulls
- `pass-080/enc-stream-mt8.log` (`matrix --mode enc-stream --mt-workers 8
  --shape json,text,jsonreal,textreal --file bench/big/dll100.raw`) for
  the stream-mt8 figure

The encode pareto takes no cell from a pass's parallel ST sections: two
cells at a time move zstdx's mid cells by up to 30 percent against serial
(docs/src/dev/bench/methodology.md). The decode and the stream-mt8
sections never parallelize.

The encode figure draws each side as a staircase, the throughput of its
fastest level at every density, and shades the region between the two
staircases in the color of the side that leads there. A band label is
the throughput
multiple at equal density. It lies upright at the vertical center of its
band, and adjacent bands too narrow for a label of their own share one
that carries their range. Every level is a marker; the six tier levels
always carry their number, and any other level carries its number when the
panel has room.

Speedups are libzstd_time / ours_time: >1 we are faster. Run from the repo
root: uv run --with matplotlib bench/plot_readme.py
"""
import math
import re
from itertools import pairwise
from pathlib import Path
from typing import NamedTuple

import matplotlib
import matplotlib.patches as mpatches
import matplotlib.path as mpath
import matplotlib.patheffects as pe
import matplotlib.pyplot as plt
from matplotlib.font_manager import FontProperties
from matplotlib.text import Text
from matplotlib.ticker import FixedLocator, NullLocator, ScalarFormatter
from matplotlib.transforms import Bbox

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
DATA = ROOT / "bench" / "data"

OURS = "#1d4ed8"
ZSTD = "#6b7280"
OURS_BAND = "#bfdbfe"
ZSTD_BAND = "#d1d5db"
LEVELS = range(1, 23)
# The ladder tiers (1, 3, 9, 13, 17, 19): the levels the prose names.
TIERS = (1, 3, 9, 13, 17, 19)

# Raw size of each encode payload; a log's frame sizes divide into ratios.
RAW_BYTES = {"json": 32 << 20, "text": 32 << 20, "jsonreal": 32 << 20, "textreal": 32 << 20, "dll100": 100 << 20}

# One encode panel: the payload name at the axes top, the corpus
# provenance note above it. The levels join at
# plot time from the stream ladder logs.
ENCODE = {
    "jsonreal": {
        "title": "json",
        "note": "real corpus · GitHub Archive event JSON · 32 MiB",
        "xlim": (7.5, 14.0), "ylim": (5, 1300),
        "xticks": (8, 9, 10, 11, 12, 13, 14), "yticks": (5, 10, 20, 50, 100, 200, 500, 1000),
    },
    "textreal": {
        "title": "text",
        "note": "real corpus · Wikipedia XML (enwik8) · 32 MiB",
        "xlim": (2.3, 3.95), "ylim": (2, 700),
        "xticks": (2.5, 3.0, 3.5), "yticks": (2, 5, 10, 20, 50, 100, 200, 500),
    },
    "dll100": {
        "title": "dll100",
        "note": "system ELF binaries · 100 MiB",
        "xlim": (2.0, 6.7), "ylim": (5, 900),
        "xticks": (3, 4, 5, 6), "yticks": (5, 10, 20, 50, 100, 200, 500),
    },
}

# A band at the end of the shaded region narrower than this share of the
# axis stays unfilled.
MIN_BAND = 0.004
# Level-number slots in points around a marker: the home side (right of a
# zstdx dot, left of a libzstd square), above and below it, straight up and
# straight down, then the far side.
NUMBER_SLOTS = ((7, 0), (7, 7), (7, -7), (0, 8), (0, -8), (-7, 0), (-7, 7), (-7, -7))

# Streaming-encode MT(8) speedup (ours/zstd throughput, both sides at 8
# workers, 64 KiB pulls) per tier: payload -> (x at each of the 6 tiers).
# Each value is the inverse of the harness's median per-round time ratio.
# The best row carries the Dense-class verdict round: the binary payload's
# wide ring collapsed the tier 27x before it (see matchers.md).
ENCODE_MT = {
    "fastest": {"jsonreal": 0.61, "textreal": 0.40, "dll100": 0.53},
    "fast": {"jsonreal": 1.73, "textreal": 2.58, "dll100": 2.00},
    "balanced": {"jsonreal": 2.11, "textreal": 0.88, "dll100": 0.84},
    "best": {"jsonreal": 1.31, "textreal": 1.15, "dll100": 0.71},
    "opt": {"jsonreal": 4.50, "textreal": 3.62, "dll100": 2.11},
    "ultra": {"jsonreal": 3.80, "textreal": 2.87, "dll100": 1.60},
}

# Streaming-decode speedup (libzstd_time/ours_time) per corpus cell:
# (label, (min, at-64KiB, max) across the 4 KiB-1 MiB pull sweep,
# mt16-vs-zstd-stream or None). Both sides single-threaded; the reader loop
# pulls `pull` bytes per read. >1 = zstdx faster.
DECODE = [
    ("jsonreal zst1", (1.066, 1.070, 1.077), None),
    ("jsonreal zst3", (1.028, 1.035, 1.042), None),
    ("jsonreal zst9", (1.029, 1.036, 1.039), None),
    ("textreal zst1", (0.983, 0.985, 0.989), None),
    ("textreal zst3", (0.896, 0.899, 0.903), None),
    ("textreal zst9", (0.887, 0.893, 0.895), None),
    ("dll100 zst1", (1.007, 1.015, 1.023), None),
    ("dll100 zst3", (0.976, 0.980, 0.986), None),
    ("dll100 zst9", (0.971, 0.976, 0.982), None),
    ("dll100 zst19", (0.965, 0.970, 0.973), None),
    ("jsonreal zst3 · 16 threads", (None, 1.45, None), True),
    ("textreal zst3 · 16 threads", (None, 2.17, None), True),
    ("dll100 zst3 · 16 threads", (None, 1.88, None), True),
]


class Cell(NamedTuple):
    """One interleaved A/B cell of a `matrix` log: frame sizes in bytes,
    throughput in MiB/s of raw input, and the per-round time ratio of ours
    over libzstd (median, MAD, min, max) over `rounds` rounds."""
    ruz: int
    zstd: int
    ours: float
    ref: float
    x: float
    mad: float
    lo: float
    hi: float
    rounds: int


# A cell's frame sizes (`(r ..)` is the bulk form), then its A/B line.
SIZES = re.compile(r"^sizes (\S+)\s+ruz\s+(\d+)\s+(?:\(r\s+[\d.]+\)\s+)?zstd\s+(\d+)")
CELL = re.compile(
    r"^(\S+)\s+([\d.]+)\s+([\d.]+)\s+x\s*([\d.]+)\s*±([\d.]+)\s+\[([\d.]+)\.\.([\d.]+)\]\s+n=(\d+)"
)


def read_cells(*logs: Path) -> dict[str, Cell]:
    """The cells of `matrix` logs by name."""
    cells: dict[str, Cell] = {}
    for log in logs:
        sizes: tuple[str, int, int] | None = None
        for line in log.read_text().splitlines():
            if m := SIZES.match(line):
                sizes = (m[1], int(m[2]), int(m[3]))
            elif (m := CELL.match(line)) and sizes is not None and sizes[0] == m[1]:
                cells[m[1]] = Cell(sizes[1], sizes[2], *map(float, m.group(2, 3, 4, 5, 6, 7)), int(m[8]))
                sizes = None
    return cells


def load_encode() -> None:
    """Fill each encode panel with (ratio, MiB/s) per level, both sides."""
    cells = read_cells(*sorted((DATA / "stream-ladder-20261126").glob("*.log")))
    for payload, cfg in ENCODE.items():
        cfg["zstdx"], cfg["libzstd"] = {}, {}
        raw = RAW_BYTES[payload]
        for level in LEVELS:
            cell = cells[f"{payload}.l{level}.stream"]
            cfg["zstdx"][level] = (raw / cell.ruz, cell.ours)
            cfg["libzstd"][level] = (raw / cell.zstd, cell.ref)


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


def reach(front: list[tuple[float, float]], ratio: float) -> float | None:
    """Throughput of the fastest level at least `ratio` dense."""
    for r, s in front:
        if ratio <= r:
            return s
    return None


def bands(
    ours: list[tuple[float, float]], ref: list[tuple[float, float]], xlo: float
) -> list[list[float | None]]:
    """Density intervals between the merged frontier steps: [lo, hi, ours, ref]."""
    cuts = sorted({xlo} | {r for r, _ in ours} | {r for r, _ in ref})
    out: list[list[float | None]] = []
    for lo, hi in pairwise(cuts):
        mid = (lo + hi) / 2
        cell = [lo, hi, reach(ours, mid), reach(ref, mid)]
        if out and out[-1][2:] == cell[2:]:
            out[-1][1] = hi
        else:
            out.append(cell)
    return out


def stair(front: list[tuple[float, float]], xlo: float) -> tuple[list[float], list[float]]:
    """Step outline of `reach`: flat from `xlo`, dropping at each frontier level."""
    xs = [xlo] + [r for r, _ in front[:-1]] + [front[-1][0]]
    ys = [s for _, s in front] + [front[-1][1]]
    return xs, ys


def stair_path(ax, front: list[tuple[float, float]], xlo: float) -> mpath.Path:
    """The staircase as `plot_encode` draws it, in display coordinates: the
    steps-post form of `stair`, which holds each level up to the next drop.
    A `Line2D` of a steps drawstyle hands out its unstepped vertices."""
    xs, ys = stair(front, xlo)
    x = [v for pair in pairwise(xs) for v in pair] + [xs[-1]]
    y = [v for level in ys[:-1] for v in (level, level)] + [ys[-1]]
    return mpath.Path(ax.transData.transform(list(zip(x, y))))


def shaded(rows: list[list], width: float) -> list[list]:
    """The bands of the shaded region. A hairline band at either end stays
    unfilled; one inside stays filled, because a gap would open between its
    neighbours."""
    def hairline(row: list) -> bool:
        return (row[1] - row[0]) / width < MIN_BAND

    first, last = 0, len(rows)
    while first < last and hairline(rows[first]):
        first += 1
    while last > first and hairline(rows[last - 1]):
        last -= 1
    return rows[first:last]


def steps(rows: list[list], top, floor) -> tuple[list, list]:
    """Polygon over `rows`: along the top, back along the floor."""
    xs, hi, lo = [], [], []
    for a, b, z, r in rows:
        xs += [a, b]
        hi += [top(z, r)] * 2
        lo += [floor(z, r)] * 2
    return xs + xs[::-1], hi + lo[::-1]


def shade(ax, rows: list[list], ylo: float) -> None:
    """Fill the region between the two staircases: gray throughout, blue
    where zstdx leads. Each stretch is one polygon, so no antialiasing seam
    shows inside it."""
    def top(z, r):
        return r if z is None else z if r is None else max(z, r)

    def floor(z, r):
        return ylo if z is None or r is None else min(z, r)

    ax.fill(*steps(rows, top, floor), color=ZSTD_BAND, lw=0, zorder=1)
    lead: list[list] = []
    for row in rows + [None]:
        # a stretch is contiguous in density
        if row is not None and row[2] is not None and (row[3] is None or row[2] > row[3]):
            lead.append(row)
            continue
        if lead:
            ax.fill(*steps(lead, lambda z, r: z, floor), color=OURS_BAND, lw=0, zorder=1)
        lead = []


def band_label(mult: float) -> str:
    """One multiple below ten with a decimal, an integer above."""
    return f"{mult:.0f}×" if mult >= 10 else f"{mult:.1f}×"


def label_bands(fig, ax, rows: list[list], taken: list[Bbox]) -> None:
    """Write the throughput multiple between the two staircases.

    A label lies upright at the vertical center of its band. A band too
    narrow for its label joins its bare neighbours of the same leader in a
    range label, which takes the largest rectangle that all of them cover.
    """
    px_per_pt = fig.dpi / 72

    renderer = fig.canvas.get_renderer()
    # a label keeps this gap to the stair lines on its edges (half the line width)
    gap = 1.5 * px_per_pt
    bold = FontProperties(weight="bold")

    def room_of(lo: float, low: float, hi: float, high: float) -> Bbox:
        """The display rectangle of a data rectangle."""
        (x0, y0), (x1, y1) = ax.transData.transform([(lo, low), (hi, high)])
        return Bbox.from_extents(x0, y0, x1, y1)

    def place(label: str, room: Bbox, ours_leads: bool) -> bool:
        """Put `label` at the center of `room`, flush with either side when
        the center is taken, and shrink before giving up. The test and the
        centering use the painted glyphs: the layout box of a line reaches
        below the baseline, where a digit paints nothing."""
        if room.width <= 2 * gap or room.height <= 2 * gap:
            return False
        inner = Bbox.from_extents(room.x0 + gap, room.y0 + gap, room.x1 - gap, room.y1 - gap)
        for size in (9, 8.5, 8):
            bold.set_size(size)
            w, h, d = renderer.get_text_width_height_descent(label, bold, False)
            # the line box is at least as tall as "lp" and its baseline carries the glyphs
            _, lp_h, lp_d = renderer.get_text_width_height_descent("lp", bold, False)
            base = -max(h, lp_h) / 2 + max(d, lp_d)
            # the anchor sits below the glyph center by as much as the glyphs sit above it
            cy = (room.y0 + room.y1) / 2 - (base + (h - d) / 2)
            ink = Bbox.from_extents(-w / 2, base - d, w / 2, base + h)
            centers = [(room.x0 + room.x1) / 2]
            # flush with either side when the band is wider than the label
            centers += ([inner.x0 + w / 2, inner.x1 - w / 2] if ink.width < inner.width else [])
            for cx in centers:
                trial = ink.translated(cx, cy)
                inside = inner.contains(trial.x0, trial.y0) and inner.contains(trial.x1, trial.y1)
                if inside and not any(trial.padded(px_per_pt).overlaps(other) for other in taken):
                    ax.text(
                        *ax.transData.inverted().transform((cx, cy)), label,
                        ha="center", va="center", fontsize=size, fontweight="bold",
                        color=OURS if ours_leads else "#4b5563", zorder=4,
                    )
                    taken.append(trial.padded(px_per_pt))
                    return True
        return False

    def group(bare: list[list], ours_leads: bool) -> None:
        """Range-label a stretch of bare bands: the largest rectangle first,
        then the stretches on either side of it."""
        windows = []
        for i, (lo, *_) in enumerate(bare):
            low, high = 0.0, math.inf
            for j, (_, hi, z, r) in enumerate(bare[i:], start=i):
                if j > i and bare[j - 1][1] != bare[j][0]:
                    # a labelled band sits in between
                    break
                low, high = max(low, min(z, r)), min(high, max(z, r))
                if low >= high:
                    break
                if j > i:
                    windows.append(((hi - lo) * math.log(high / low), i, j, lo, low, hi, high))
        for _, i, j, lo, low, hi, high in sorted(windows, reverse=True):
            mults = [max(z, r) / min(z, r) for _, _, z, r in bare[i:j + 1]]
            label = band_label(max(mults)) if min(mults) == max(mults) else f"{band_label(min(mults))[:-1]}–{band_label(max(mults))}"
            if place(label, room_of(lo, low, hi, high), ours_leads):
                group(bare[:i], ours_leads)
                group(bare[j + 1:], ours_leads)
                return

    # stretches of bands contiguous in density and led by the same side
    runs: list[list] = []
    for row in rows + [None]:
        leads = row is not None and None not in row[2:] and row[2] != row[3]
        joins = (
            leads and runs and runs[-1][-1][1] == row[0]
            and (runs[-1][0][2] > runs[-1][0][3]) == (row[2] > row[3])
        )
        if joins:
            runs[-1].append(row)
        elif leads:
            runs.append([row])
    for run in runs:
        ours_leads = run[0][2] > run[0][3]
        bare = []
        for lo, hi, z, r in run:
            low, high = min(z, r), max(z, r)
            if not place(band_label(high / low), room_of(lo, low, hi, high), ours_leads):
                bare.append([lo, hi, z, r])
        group(bare, ours_leads)


def take_markers(fig, ax, cfg: dict, taken: list[Bbox]) -> None:
    """Reserve every marker's box. A libzstd level left of the panel becomes
    a clipped marker with a note on the ratio it sits at."""
    px_per_pt = fig.dpi / 72
    for level in LEVELS:
        for side in ("zstdx", "libzstd"):
            x, y = cfg[side][level]
            if x < cfg["xlim"][0]:
                ax.plot(cfg["xlim"][0], y, marker="<", color=ZSTD, ms=7, clip_on=False, zorder=6)
                note = ax.annotate(
                    f"{level} at {x:.1f}", (cfg["xlim"][0], y), xytext=(9, 0), textcoords="offset points",
                    ha="left", va="center", fontsize=7.5, color=ZSTD,
                )
                taken.append(note.get_window_extent().padded(0.5 * px_per_pt))
                continue
            mx, my = ax.transData.transform((x, y))
            # the marker with its white rim
            taken.append(Bbox.from_bounds(mx - 4.5 * px_per_pt, my - 4.5 * px_per_pt, 9 * px_per_pt, 9 * px_per_pt))


def number_levels(fig, ax, cfg: dict, stairs: list[mpath.Path], taken: list[Bbox]) -> None:
    """Number every level whose marker has room for it.

    The six tier levels go first and always carry their number, then the
    frontier levels. Every other level takes its first slot clear of every
    marker, band label, note and staircase (`stairs`, display coordinates)
    inside the panel, and stays unnumbered when none is or when a level of
    the same side on the same spot is numbered.
    """
    px_per_pt = fig.dpi / 72
    xlo = cfg["xlim"][0]
    front = {side: frontier(cfg[side]) for side in ("zstdx", "libzstd")}
    area = ax.get_window_extent()

    def number(side: str, level: int, dx: float, dy: float) -> Text:
        return ax.annotate(
            str(level), cfg[side][level], xytext=(dx, dy), textcoords="offset points",
            ha="left" if dx > 0 else "right" if dx < 0 else "center",
            va="bottom" if dy > 0 else "top" if dy < 0 else "center",
            fontsize=7.5, color=OURS if side == "zstdx" else ZSTD, zorder=6,
            path_effects=[pe.withStroke(linewidth=2, foreground="white")],
        )

    numbered: dict[str, list[Bbox]] = {"zstdx": [], "libzstd": []}
    pairs = sorted(
        ((side, level) for side in numbered for level in LEVELS),
        key=lambda p: (p[1] not in TIERS, cfg[p[0]][p[1]] not in front[p[0]], p[1], p[0]),
    )
    for side, level in pairs:
        x, y = cfg[side][level]
        if x < xlo:
            continue
        mx, my = ax.transData.transform((x, y))
        marker = Bbox.from_bounds(mx - 4.5 * px_per_pt, my - 4.5 * px_per_pt, 9 * px_per_pt, 9 * px_per_pt)
        if level not in TIERS and any(marker.overlaps(other) for other in numbered[side]):
            continue
        home = 1 if side == "zstdx" else -1
        slots = [(home * dx, dy) for dx, dy in NUMBER_SLOTS]
        for dx, dy in slots:
            num = number(side, level, dx, dy)
            box = num.get_window_extent().padded(0.5 * px_per_pt)
            # an annotation is not clipped, so a number near the top would leave the panel
            inside = area.containsx(box.x0) and area.containsx(box.x1) and area.containsy(box.y0) and area.containsy(box.y1)
            on_line = any(line.intersects_bbox(box, filled=False) for line in stairs)
            num.remove()
            if inside and not on_line and not any(box.overlaps(other) for other in taken):
                number(side, level, dx, dy)
                taken.append(box)
                break
        else:
            if level not in TIERS:
                continue
            # a tier number takes its home slot over a line or a neighbour
            number(side, level, *slots[0])
        numbered[side].append(marker)


def plot_encode() -> None:
    load_encode()
    fig, axes = plt.subplots(1, 3, figsize=(12.4, 4.8), constrained_layout=True)
    panels = []
    for ax, cfg in zip(axes, ENCODE.values()):
        xlo, xhi = cfg["xlim"]
        ylo = cfg["ylim"][0]
        ours, ref = frontier(cfg["zstdx"]), frontier(cfg["libzstd"])
        rows = bands(ours, ref, xlo)
        shade(ax, shaded(rows, xhi - xlo), ylo)
        if (ours[-1][0] - ref[-1][0]) / (xhi - xlo) >= MIN_BAND:
            # no libzstd level is denser than the line
            ax.axvline(ref[-1][0], color=ZSTD, ls=":", lw=1.0, zorder=2)
            ax.annotate(
                "no libzstd level\nthis dense", (ref[-1][0], 0.85), xycoords=("data", "axes fraction"),
                xytext=(5, 0), textcoords="offset points", ha="left", va="center",
                fontsize=7.5, style="italic", color="#4b5563",
            )
        for front, color, ls, lw in ((ours, OURS, "-", 2.0), (ref, ZSTD, "--", 1.6)):
            ax.plot(*stair(front, xlo), drawstyle="steps-post", color=color, ls=ls, lw=lw, zorder=3)
        for side, color, marker in (("zstdx", OURS, "o"), ("libzstd", ZSTD, "s")):
            pts = cfg[side]
            ax.scatter(
                [pts[l][0] for l in LEVELS], [pts[l][1] for l in LEVELS],
                s=24, color=color, marker=marker, zorder=5, edgecolors="white", linewidths=0.5,
            )
        ax.set_yscale("log")
        ax.set_xlim(xlo, xhi)
        ax.set_ylim(*cfg["ylim"])
        ax.xaxis.set_major_locator(FixedLocator(cfg["xticks"]))
        ax.yaxis.set_major_locator(FixedLocator(cfg["yticks"]))
        ax.yaxis.set_major_formatter(ScalarFormatter())
        ax.xaxis.set_minor_locator(NullLocator())
        ax.yaxis.set_minor_locator(NullLocator())
        # the grid sits under the bands, which stay flat fields of colour
        ax.set_axisbelow(True)
        ax.grid(axis="y", which="major", alpha=0.3, lw=0.5)
        ax.tick_params(labelsize=8.5)
        # the payload name sits at the axes top; the provenance note rides
        # above it as the axes title, lifted clear of the name by its pad
        ax.set_title(cfg["note"], fontsize=7.5, color="#6b7283", pad=20)
        ax.text(
            0.5, 1.0, cfg["title"], transform=ax.transAxes,
            ha="center", va="bottom", fontsize=11,
        )
        ax.text(
            0.97, 0.95, "better ↗", transform=ax.transAxes, ha="right", va="top",
            fontsize=9, style="italic", color="#4b5563",
        )
        ax.set_xlabel("compression ratio", fontsize=9)
        panels.append((ax, cfg, rows))
    axes[0].set_ylabel("encode throughput (MiB/s, log)", fontsize=9)
    handles = [
        plt.Line2D([], [], ls="none", marker="o", ms=6, color=OURS, label="zstdx level 1–22"),
        plt.Line2D([], [], ls="none", marker="s", ms=6, color=ZSTD, label="libzstd level 1–22"),
        mpatches.Patch(color=OURS_BAND, label="zstdx leads"),
        mpatches.Patch(color=ZSTD_BAND, label="libzstd leads"),
    ]
    # title rides inside the legend block so constrained layout keeps them
    # together (a separate suptitle collides with the outside legend row)
    fig.legend(
        handles=handles, loc="outside upper center", ncol=4, frameon=False,
        fontsize=9, alignment="center", title_fontsize=11.5,
        title="Streaming encode (64 KiB pulls): throughput vs compression ratio, single-threaded",
    )
    fig.supxlabel(
        "shaded bands: the throughput gap between the two sides at equal density, colored by the side that leads; "
        "band labels: the throughput multiple at equal density, a range when one label spans several bands\n"
        "every level 1–22, single-threaded unknown-size streams at 64 KiB pulls, "
        "one cell on the machine at a time",
        fontsize=8, color="#6b7283",
    )
    # the labels test their fit against the laid-out axes, which exist after a draw
    fig.draw_without_rendering()
    px_per_pt = fig.dpi / 72
    for ax, cfg, rows in panels:
        # band labels claim the center of their band first, level numbers fit around them
        taken = [t.get_window_extent().padded(0.5 * px_per_pt) for t in ax.texts]
        take_markers(fig, ax, cfg, taken)
        label_bands(fig, ax, rows, taken)
        xlo = cfg["xlim"][0]
        stairs = [stair_path(ax, frontier(cfg[side]), xlo) for side in ("zstdx", "libzstd")]
        number_levels(fig, ax, cfg, stairs, taken)
    fig.savefig(ASSETS / "encode-pareto.svg", bbox_inches="tight")
    plt.close(fig)


def plot_encode_mt() -> None:
    payloads = {"jsonreal": "#1d4ed8", "textreal": "#3b82f6", "dll100": "#93c5fd"}
    n = len(ENCODE_MT)
    fig, ax = plt.subplots(figsize=(9.6, 4.6), constrained_layout=True)
    ys = list(range(n, 0, -1))
    jitter = {"jsonreal": 0.22, "textreal": 0.0, "dll100": -0.22}
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
    fig, ax = plt.subplots(figsize=(10.6, 5.4), constrained_layout=True)
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
    # edge from the widest value on the chart (textreal mt16 2.2x this pass)
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
    # file output only; the label placement measures text on this renderer
    matplotlib.use("Agg")
    ASSETS.mkdir(exist_ok=True)
    plot_encode()
    plot_encode_mt()
    plot_decode()
    for name in ("encode-pareto", "encode-stream-mt", "decode-speedup"):
        print(f"wrote {ASSETS / (name + '.svg')}")


if __name__ == "__main__":
    main()
