#!/usr/bin/env python3
"""Regenerates PlumSpirit's official branding assets (B1-0175 / B1-0176) from the
owner's artwork, which lives in the v1.0-beta1 proposal the owner supplied:

    docs/contracts/v1.0-beta1_proposal.docx
        word/media/image17.png   the official logo (the plum alone)
        word/media/image9.png    the logo with the wordmark ("Plum" + "Spirit")

Both are read straight from the .docx (checked against their SHA-256 below, so a
changed source is never processed silently) and never modified. Outputs:

    assets/brand/plumspirit_logo.png        512 px, transparent, square
    assets/brand/logo_{32,64,128,256}.png   the sizes the app embeds
    assets/brand/wordmark_{light,dark}_{72,144,288}.png
                                            the wordmark by height (light theme:
                                            the owner's colors; dark theme: the
                                            letters lightened for contrast)
    assets/icon/plumspirit.ico              16, 24, 32, 48, 64, 128, 256 px

Method (also in docs/references/v1_0_beta1/LOGO_PROVENANCE.md):
1. Background = the near-white region touching the border, plus enclosed regions
   that are near-white AND neutral (the gap under the leaf, the counters of P and
   p) and specks of a few pixels trapped at the outline where two shapes meet —
   the plum's glossy highlights are tinted and lie inside, so they stay opaque.
2. A 2.5 px band of foreground along the background is un-matted: each pixel's
   alpha is solved from P = alpha * F + (1 - alpha) * B (F: the nearest interior
   color, B: the measured background) and its color recovered as
   (P - (1 - alpha) * B) / alpha — no white fringe, no dark halo.
3. Cropped, padded, resampled with premultiplied alpha (Lanczos).

Requires Pillow, numpy and scipy. Run from the repository root:
    python3 tools/make_icons.py
"""
import colorsys
import hashlib
import io
import json
import zipfile
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

ROOT = Path(__file__).resolve().parent.parent
DOCX = ROOT / "docs" / "contracts" / "v1.0-beta1_proposal.docx"
LOGO_SRC = ("word/media/image17.png",
            "891124905b879777a4941ed8b5773eb07a0fec4da9b12e0a8cfc5fadd09d42c8")
WORD_SRC = ("word/media/image9.png",
            "3bd64540091ff6923cec7c9fc92a085cd88d54c216f84fecf15424c7d08f54c8")
BRAND = ROOT / "assets" / "brand"
ICON = ROOT / "assets" / "icon" / "plumspirit.ico"
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
APP_LOGO_SIZES = [32, 64, 128, 256]
WORD_HEIGHTS = [72, 144, 288]
# The dark theme's Piano Roll background (crates/plumspirit/src/ui/theme.rs).
DARK_BG = (0x22, 0x23, 0x27)


def source(member: str, digest: str) -> np.ndarray:
    data = zipfile.ZipFile(DOCX).read(member)
    got = hashlib.sha256(data).hexdigest()
    if got != digest:
        raise SystemExit(f"{member} changed (SHA-256 {got}); check the owner's artwork first")
    return np.asarray(Image.open(io.BytesIO(data)).convert("RGB")).astype(np.float64)


def cutout(rgb: np.ndarray) -> np.ndarray:
    """Straight-alpha RGBA (floats 0 - 255) of the artwork without its background."""
    near_white = rgb.min(axis=2) >= 240
    lab, n = ndimage.label(near_white)
    border = set(np.unique(np.concatenate([lab[0], lab[-1], lab[:, 0], lab[:, -1]]))) - {0}
    bg = np.isin(lab, list(border))
    sizes = ndimage.sum(np.ones_like(lab), lab, index=range(1, n + 1))
    for i in range(1, n + 1):
        if i in border or sizes[i - 1] < 30:
            continue
        m = rgb[lab == i].mean(axis=0)
        if m.min() >= 250 and m.max() - m.min() <= 3:
            bg |= lab == i
    # Specks of the white background trapped where two shapes meet at the
    # outline (a few pixels within 8 px of the background) are background too;
    # highlights lie deeper inside the shapes.
    near_bg = ndimage.distance_transform_edt(~bg) <= 8
    for i in range(1, n + 1):
        if i not in border and sizes[i - 1] <= 12 and near_bg[lab == i].all():
            bg |= lab == i
    b = np.median(rgb[bg], axis=0)
    fg = ~bg
    dist = ndimage.distance_transform_edt(fg)
    band = fg & (dist <= 2.5)
    interior = fg & ~band
    _, idx = ndimage.distance_transform_edt(~interior, return_indices=True)
    f = rgb[idx[0], idx[1]]
    num = ((rgb - b) * (f - b)).sum(axis=2)
    den = ((f - b) ** 2).sum(axis=2)
    alpha = np.where(den > 1e-6, np.clip(num / np.maximum(den, 1e-6), 0.0, 1.0), 0.0)
    alpha = np.where(interior, 1.0, np.where(band, alpha, 0.0))
    a3 = alpha[..., None]
    unmatted = np.clip((rgb - (1.0 - a3) * b) / np.maximum(a3, 1e-6), 0, 255)
    color = np.where(a3 >= 0.2, unmatted, f)
    color = np.where(interior[..., None], rgb, color)
    return np.dstack([color, alpha * 255.0])


def to_image(rgba: np.ndarray) -> Image.Image:
    return Image.fromarray(np.clip(np.round(rgba), 0, 255).astype(np.uint8), "RGBA")


def crop(img: Image.Image, margin: float, square: bool) -> Image.Image:
    a = np.asarray(img)[..., 3]
    ys, xs = np.nonzero(a > 4)
    img = img.crop((xs.min(), ys.min(), xs.max() + 1, ys.max() + 1))
    w, h = img.size
    pad = int(round(max(w, h) * margin))
    side_w, side_h = (max(w, h) + 2 * pad,) * 2 if square else (w + 2 * pad, h + 2 * pad)
    out = Image.new("RGBA", (side_w, side_h), (0, 0, 0, 0))
    out.paste(img, ((side_w - w) // 2, (side_h - h) // 2))
    return out


def resize(img: Image.Image, size) -> Image.Image:
    """Lanczos on premultiplied alpha (no dark or white halo at the edge)."""
    return img.convert("RGBa").resize(size, Image.LANCZOS).convert("RGBA")


def luminance(c) -> float:
    def ch(v):
        v /= 255.0
        return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = (ch(float(x)) for x in c)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b) -> float:
    x, y = luminance(a), luminance(b)
    return (max(x, y) + 0.05) / (min(x, y) + 0.05)


def dark_variant(word: np.ndarray, split: int):
    """The wordmark for the dark theme: the letters right of `split` lightened
    (hue kept): "Plum" (dark) becomes a pale lavender, "Spirit" its own purple
    lightened until it reads at 4.5 : 1 on the dark background. The plum stays
    as drawn. Returns the image and the two colors' contrast ratios."""
    out = word.copy()
    rgb = out[..., :3] / 255.0
    a = out[..., 3]
    region = np.zeros(a.shape, dtype=bool)
    region[:, split:] = True
    region &= a > 0
    lum = rgb.max(axis=2) * 0.5 + rgb.min(axis=2) * 0.5
    heavy = region & (lum < 0.33)
    light = region & ~heavy
    ratios = {}
    for mask, name in ((heavy, "Plum"), (light, "Spirit")):
        if not mask.any():
            continue
        base = rgb[mask & (a > 250)].mean(axis=0)
        h, l, s = colorsys.rgb_to_hls(*base)
        if name == "Plum":
            target = colorsys.hls_to_rgb(h, 0.88, min(s, 0.55))
        else:
            target = base
            lt = l
            while contrast(tuple(c * 255 for c in target), DARK_BG) < 4.5 and lt < 0.95:
                lt += 0.01
                target = colorsys.hls_to_rgb(h, lt, s)
        # Every pixel of the group takes the target, its own lightness offset
        # from the group's mean kept (anti-aliased edges keep their alpha).
        rgb[mask] = np.clip(np.array(target) + (rgb[mask] - base) * 0.5, 0, 1)
        ratios[name] = round(contrast(tuple(int(c * 255) for c in target), DARK_BG), 2)
    out[..., :3] = rgb * 255.0
    return out, ratios


def text_split(word: Image.Image) -> int:
    """The first empty column after the plum (the gap before the letters)."""
    a = np.asarray(word)[..., 3]
    cols = (a > 8).any(axis=0)
    start = int(np.argmax(cols))
    x = start
    while x < len(cols) and cols[x]:
        x += 1
    return x


def sha(path: Path) -> str:
    """SHA-256 of the decoded RGBA pixels (PNG) or of the file (ICO): the
    pixel hash survives metadata chunks a file transfer may add."""
    if path.suffix == ".png":
        im = Image.open(path).convert("RGBA")
        return hashlib.sha256(f"{im.size}".encode() + im.tobytes()).hexdigest()
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    BRAND.mkdir(parents=True, exist_ok=True)
    ICON.parent.mkdir(parents=True, exist_ok=True)
    report = {}
    logo = crop(to_image(cutout(source(*LOGO_SRC))), 0.03, True)
    master = resize(logo, (512, 512))
    master.save(BRAND / "plumspirit_logo.png", optimize=True)
    for s in APP_LOGO_SIZES:
        resize(logo, (s, s)).save(BRAND / f"logo_{s}.png", optimize=True)
    icons = [resize(logo, (s, s)) for s in ICO_SIZES]
    icons[-1].save(ICON, sizes=[(s, s) for s in ICO_SIZES], append_images=icons[:-1])
    word_rgba = cutout(source(*WORD_SRC))
    word = crop(to_image(word_rgba), 0.02, False)
    split = text_split(word)
    dark, ratios = dark_variant(np.asarray(word).astype(np.float64), split)
    dark_img = to_image(dark)
    w, h = word.size
    for hh in WORD_HEIGHTS:
        ww = int(round(w * hh / h))
        resize(word, (ww, hh)).save(BRAND / f"wordmark_light_{hh}.png", optimize=True)
        resize(dark_img, (ww, hh)).save(BRAND / f"wordmark_dark_{hh}.png", optimize=True)
    report["wordmark_aspect"] = round(w / h, 4)
    report["dark_contrast"] = ratios
    report["outputs"] = {
        str(p.relative_to(ROOT)): sha(p)
        for p in sorted(BRAND.glob("*.png")) + [ICON]
    }
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
