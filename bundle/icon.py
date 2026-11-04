"""rtty's app icon: a bold ❯_ prompt on a dark rounded square, in One Dark Pro's colors.

Drawn from shapes, no fonts, at 4x and downsampled for antialiasing:
    uv run --with pillow bundle/icon.py OUTDIR
writes OUTDIR/icon-1024.png, the icon, and OUTDIR/icon-sizes.png, the icon at 16 to 256 px on
light and dark, to check it still reads small. `just icon` turns the first into bundle/rtty.icns.
"""

import math
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

SIZE, SCALE = 1024, 4  # macOS's app icon grid: a 1024 canvas,
BODY = 824  # an 824 square with continuous corners in its middle,
SHADOW = (12, 20, 0.35)  # a soft shadow in the margin: y offset, blur, opacity
TOP, BOTTOM, RIM = "#282C34", "#21252B", 0.12  # RIM: the opacity of the white edge
PROMPT, CURSOR = "#98C379", "#ABB2BF"
STROKE, ROUNDING = 116, 8
SHEET = (16, 32, 64, 128, 256)


def squircle(inset=0, exponent=5, points=1440):
    """The superellipse |x|^5 + |y|^5 = 1: continuous corners, near a 185 px radius at 824."""
    r, middle = BODY / 2 - inset, SIZE / 2

    def curve(c):
        return middle + r * math.copysign(abs(c) ** (2 / exponent), c)

    angles = [2 * math.pi * i / points for i in range(points)]
    return [(curve(math.cos(a)), curve(math.sin(a))) for a in angles]


def chevron(left, top, height, angle):
    """❯: two arms of STROKE meeting in a point, their ends cut level."""
    cut, run = STROKE / math.sin(angle), height / 2 / math.tan(angle)
    middle, bottom = top + height / 2, top + height
    return [
        (left, top), (left + cut, top), (left + cut + run, middle),
        (left + cut, bottom), (left, bottom), (left + run, middle),
    ]  # fmt: skip


def bar(left, top, width, height):
    return [(left, top), (left + width, top), (left + width, top + height), (left, top + height)]


def mask(polygon, rounding=0):
    """The polygon's coverage at SCALE; blurring and cutting at half rounds its corners."""
    image = Image.new("L", (SIZE * SCALE,) * 2, 0)
    ImageDraw.Draw(image).polygon([(x * SCALE, y * SCALE) for x, y in polygon], fill=255)
    blurred = image.filter(ImageFilter.GaussianBlur(rounding * SCALE))
    return blurred.point(lambda v: 255 * (v > 127)) if rounding else image


def layer(fill, coverage):
    image = Image.new("RGBA", coverage.size, fill)
    image.putalpha(coverage)
    return image


def icon():
    height, angle, gap, width, thickness = 400, math.radians(54), 40, 236, 72
    chevron_width = STROKE / math.sin(angle) + height / 2 / math.tan(angle)
    left, top = (SIZE - chevron_width - gap - width) / 2, (SIZE - height) / 2
    body = mask(squircle())
    ramp = Image.linear_gradient("L").resize((BODY * SCALE,) * 2)
    gradient = Image.new("L", body.size, 0)
    gradient.paste(ramp, ((SIZE - BODY) * SCALE // 2,) * 2)
    face = Image.composite(layer(BOTTOM, body), layer(TOP, body), gradient)
    rim = ImageChops.subtract(body, mask(squircle(inset=4))).point(lambda v: round(v * RIM))
    cursor = bar(left + chevron_width + gap, top + height - thickness, width, thickness)
    for fill, coverage in [
        ("white", rim),
        (PROMPT, mask(chevron(left, top, height, angle), ROUNDING)),
        (CURSOR, mask(cursor, ROUNDING)),
    ]:
        face = Image.alpha_composite(face, layer(fill, coverage))
    face = face.resize((SIZE, SIZE), Image.Resampling.BOX)
    offset, blur, opacity = SHADOW
    shade = face.getchannel("A").point(lambda a: round(a * opacity))
    shade = shade.transform(face.size, Image.Transform.AFFINE, (1, 0, 0, 0, 1, -offset))
    return Image.alpha_composite(layer("black", shade.filter(ImageFilter.GaussianBlur(blur))), face)


def sheet(image, pad=24):
    width, row = sum(SHEET) + pad * (len(SHEET) + 1), max(SHEET) + 2 * pad
    out = Image.new("RGBA", (width, 2 * row))
    for y, color in [(0, "#ECECEC"), (row, "#1E1E1E")]:
        out.paste(color, (0, y, width, y + row))
        for i, size in enumerate(SHEET):
            small = image.resize((size, size), Image.Resampling.LANCZOS)
            out.alpha_composite(small, (pad + sum(SHEET[:i]) + i * pad, y + row - pad - size))
    return out


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: icon.py OUTDIR")
    image = icon()
    image.save(Path(sys.argv[1]) / "icon-1024.png")
    sheet(image).save(Path(sys.argv[1]) / "icon-sizes.png")
