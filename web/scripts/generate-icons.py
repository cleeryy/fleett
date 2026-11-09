#!/usr/bin/env python3
"""Render the console icons from the favicon design.

The source of truth is `public/favicon.svg`: a near-black rounded square with
three outlined cells and one filled cell. PNGs are rasterised from the same
geometry so the installed app matches the browser tab.

    python3 scripts/generate-icons.py

Requires Pillow (dev-time only, never shipped with the console).
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

PUBLIC = Path(__file__).resolve().parent.parent / "public"
ICONS = PUBLIC / "icons"

INK = (10, 10, 10, 255)  # #0a0a0a, the dark theme background
PAPER = (250, 250, 250, 255)  # #fafafa
SS = 4  # supersampling factor

# Geometry in the 32x32 space used by favicon.svg.
CELLS = [(7.5, 7.5), (17.5, 7.5), (7.5, 17.5)]
FILLED = (17.5, 17.5)
CELL = 7.0
STROKE = 2.0


def draw_icon(size: int, *, bleed: bool, glyph_scale: float = 1.0) -> Image.Image:
    """One icon.

    `bleed` fills the whole canvas (maskable / apple-touch); otherwise the
    corners stay transparent so the launcher can round them itself.
    """
    canvas = Image.new("RGBA", (size * SS, size * SS), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)
    unit = size * SS / 32

    if bleed:
        draw.rectangle((0, 0, size * SS, size * SS), fill=INK)
    else:
        draw.rounded_rectangle(
            (0, 0, size * SS - 1, size * SS - 1),
            radius=7 * unit,
            fill=INK,
        )

    offset = (32 * (1 - glyph_scale) / 2) * unit
    scale = unit * glyph_scale

    # favicon.svg strokes are centred on the path: draw the ring as an outer
    # shape plus an inner hole so the proportions match the tab icon.
    half = STROKE / 2
    for x, y in CELLS:
        draw.rounded_rectangle(
            (
                offset + (x - half) * scale,
                offset + (y - half) * scale,
                offset + (x + CELL + half) * scale,
                offset + (y + CELL + half) * scale,
            ),
            radius=(1.5 + half) * scale,
            fill=PAPER,
        )
        draw.rounded_rectangle(
            (
                offset + (x + half) * scale,
                offset + (y + half) * scale,
                offset + (x + CELL - half) * scale,
                offset + (y + CELL - half) * scale,
            ),
            radius=max(0.5, 1.5 - half) * scale,
            fill=INK,
        )

    draw.rounded_rectangle(
        (
            offset + FILLED[0] * scale,
            offset + FILLED[1] * scale,
            offset + (FILLED[0] + CELL) * scale,
            offset + (FILLED[1] + CELL) * scale,
        ),
        radius=1.5 * scale,
        fill=PAPER,
    )

    return canvas.resize((size, size), Image.LANCZOS)


def main() -> None:
    ICONS.mkdir(parents=True, exist_ok=True)
    targets = [
        ("icon-192.png", 192, False, 1.0),
        ("icon-512.png", 512, False, 1.0),
        # Maskable icons are cropped to a circle on some launchers: keep the
        # glyph inside the safe zone and bleed the background.
        ("maskable-512.png", 512, True, 0.8),
        # iOS masks the corners and wants an opaque square.
        ("apple-touch-icon.png", 180, True, 0.86),
    ]
    for name, size, bleed, glyph_scale in targets:
        image = draw_icon(size, bleed=bleed, glyph_scale=glyph_scale)
        path = ICONS / name
        image.save(path, optimize=True)
        print(f"{path.relative_to(PUBLIC.parent)}  {size}x{size}  {path.stat().st_size} B")


if __name__ == "__main__":
    main()
