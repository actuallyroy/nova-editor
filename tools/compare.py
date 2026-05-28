#!/usr/bin/env python3
"""Compare two UI screenshots: pixel match, region colours, element counts, and
a side-by-side composite.

Usage:  python tools/compare.py [reference.png] [candidate.png]
Defaults: "VSCode.png"  "Nova Editor.png"  (project root)

Why element counts: pixel-diff across two different renderers can't reach 100%
(glyph anti-aliasing differs), so it's a poor "does it match" signal. Counting
structural elements per region (activity-bar icons, sidebar rows, tabs, minimap
presence) catches *missing/extra* UI — which is what actually matters.

Outputs:
  - overall pixel match % (channel-sum diff <= THRESHOLD = match)
  - per-region average colour table (dE)
  - element-count table (reference vs candidate), flagging mismatches
  - tools/diff_heatmap.png   (red = differing pixels)
  - tools/side_by_side.png   (reference | candidate, for eyeballing)
"""
import sys
import numpy as np
from PIL import Image

THRESHOLD = 60

REGIONS = [
    ("title bar",    0.30, 0.005, 0.70, 0.022),
    ("activity bar", 0.005, 0.20, 0.022, 0.80),
    ("sidebar",      0.06, 0.20, 0.14, 0.80),
    ("tab strip",    0.30, 0.035, 0.55, 0.052),
    ("editor bg",    0.45, 0.30, 0.95, 0.70),
    ("status bar",   0.20, 0.955, 0.80, 0.972),
]


def avg_color(arr, frac, w, h):
    x0, y0 = int(w * frac[0]), int(h * frac[1])
    x1, y1 = max(x0 + 1, int(w * frac[2])), max(y0 + 1, int(h * frac[3]))
    return tuple(int(v) for v in arr[y0:y1, x0:x1].reshape(-1, 3).mean(axis=0))


def hexc(c):
    return "#%02X%02X%02X" % c


def luma(region):
    return region.astype(np.float32).mean(axis=2)


def count_bands(mask_1d):
    """Count groups of consecutive True values."""
    bands, prev = 0, False
    for v in mask_1d:
        if v and not prev:
            bands += 1
        prev = v
    return bands


def count_icons_vertical(arr, x0, x1, y0, y1, min_px=2, margin=30):
    """Count horizontally-arranged rows of content (icons/list rows) in a strip."""
    reg = luma(arr[y0:y1, x0:x1])
    bg = np.median(reg)
    rows_with_content = (reg > bg + margin).sum(axis=1) > min_px
    return count_bands(rows_with_content)


def count_tabs(arr, x0, x1, y0, y1, min_px=2, margin=40):
    """Count vertically-arranged content columns (tabs) in a horizontal strip."""
    reg = luma(arr[y0:y1, x0:x1])
    bg = np.median(reg)
    cols_with_content = (reg > bg + margin).sum(axis=0) > min_px
    # tabs are wide; collapse small gaps by counting bands of content columns
    return count_bands(cols_with_content)


def minimap_present(arr, w, h):
    """Heuristic: is the far-right editor strip non-empty (a minimap)?"""
    x0, x1 = int(w * 0.93), int(w * 0.99)
    y0, y1 = int(h * 0.12), int(h * 0.88)
    reg = luma(arr[y0:y1, x0:x1])
    bg = np.median(reg)
    frac_bright = float((reg > bg + 18).mean())
    return frac_bright > 0.02, round(frac_bright, 3)


def elements(arr, w, h):
    e = {}
    e["activity icons"] = count_icons_vertical(arr, 0, int(w * 0.028), int(h * 0.04), int(h * 0.97))
    e["sidebar rows"] = count_icons_vertical(arr, int(w * 0.03), int(w * 0.16), int(h * 0.06), int(h * 0.92))
    e["tabs"] = count_tabs(arr, int(w * 0.17), w, int(h * 0.035), int(h * 0.06))
    present, frac = minimap_present(arr, w, h)
    e["minimap"] = f"{'yes' if present else 'no'} ({frac})"
    return e


def main():
    ref_path = sys.argv[1] if len(sys.argv) > 1 else "VSCode.png"
    cand_path = sys.argv[2] if len(sys.argv) > 2 else "Nova Editor.png"
    ref = Image.open(ref_path).convert("RGB")
    cand = Image.open(cand_path).convert("RGB")
    if ref.size != cand.size:
        cand = cand.resize(ref.size)
    w, h = ref.size
    a = np.asarray(ref, dtype=np.int16)
    b = np.asarray(cand, dtype=np.int16)

    chan = np.abs(a - b).sum(axis=2)
    diff = int((chan > THRESHOLD).sum())
    total = w * h
    print(f"ref       : {ref_path}  ({w}x{h})")
    print(f"candidate : {cand_path}")
    print(f"pixel match: {100.0*(total-diff)/total:.2f}%   mean|dC|={np.abs(a-b).mean():.2f}/255")
    print()
    print(f"{'region':<13} {'reference':>10} {'candidate':>10} {'dE':>5}")
    for name, *frac in REGIONS:
        ca, cc = avg_color(a, frac, w, h), avg_color(b, frac, w, h)
        de = sum(abs(x - y) for x, y in zip(ca, cc))
        print(f"{name:<13} {hexc(ca):>10} {hexc(cc):>10} {de:>5}{'  <-- mismatch' if de>24 else ''}")

    print()
    ea, eb = elements(a, w, h), elements(b, w, h)
    print(f"{'element':<16} {'reference':>12} {'candidate':>12}")
    for k in ea:
        flag = "" if str(ea[k]) == str(eb[k]) else "  <-- differs"
        print(f"{k:<16} {str(ea[k]):>12} {str(eb[k]):>12}{flag}")

    # heatmap
    heat = np.zeros((h, w, 3), dtype=np.uint8)
    m = chan > THRESHOLD
    dim = (b.sum(axis=2) // 6).astype(np.uint8)
    heat[..., 0] = np.where(m, 255, dim)
    heat[..., 1] = np.where(m, 0, dim)
    heat[..., 2] = np.where(m, 0, dim)
    Image.fromarray(heat).save("tools/diff_heatmap.png")

    # side-by-side (half-scale to keep it light)
    sb = Image.new("RGB", (w + 8, h), (40, 40, 40))
    sb.paste(ref, (0, 0))
    sb.paste(cand, (w + 8 - w, 0)) if False else sb.paste(cand, (8 + 0, 0))
    comp = Image.new("RGB", (w * 2 + 8, h), (40, 40, 40))
    comp.paste(ref, (0, 0))
    comp.paste(cand, (w + 8, 0))
    comp = comp.resize((comp.width // 2, comp.height // 2))
    comp.save("tools/side_by_side.png")
    print("\nwrote tools/diff_heatmap.png and tools/side_by_side.png")


if __name__ == "__main__":
    main()
