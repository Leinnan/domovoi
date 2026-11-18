#!/usr/bin/env python3
"""Draws the app icons as Icon Composer documents, and exports images of them.

    python3 tools/icons.py            # writes both .icon documents
    python3 tools/icons.py export     # also renders PNGs and the Windows .ico (needs Xcode 26 and Pillow)

Two icons share one look, the domovoi (the Slavic house spirit) watching from
the dark: two ember eyes on night indigo.

- Domovoi (the Mac and iPhone app), apps/domovoi-apple/Common/AppIcon.icon: a
  hood over the eyes, with a cursor `_` for a mouth: the spirit you carry with you.
- Domovoi Server, apps/server/icon/AppIcon.icon: the eyes under a roof, the
  spirit at home, where the agents run.

The hood and eyes are the status bar glyph's (apps/server/src/tray/icon.rs).
Every shape is a filled outline rather than a stroke: Icon Composer recolours
layers for dark and tinted icons by filling their paths, which turns an open
stroked arc into a filled segment.

The documents open in Icon Composer (Xcode > Open Developer Tool) for tuning
the glass, shadows and appearances; rerunning this script overwrites that, so
carry lasting changes over to the numbers here.
"""
import json
import math
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLIENT = ROOT / "apps/domovoi-apple/Common/AppIcon.icon"
SERVER = ROOT / "apps/server/icon/AppIcon.icon"
EXPORTS = ROOT / "apps/server/icon"
ICTOOL = Path("/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool")

CREAM = "#FFF1D6"  # the hood, roof and mouth
EMBER = "#F5A524"  # the eyes, the glow of the hearth


def srgb(hex_colour):
    r, g, b = (int(hex_colour[i:i + 2], 16) / 255 for i in (1, 3, 5))
    return f"srgb:{r:.5f},{g:.5f},{b:.5f},1.00000"


def fmt(x, y):
    return f"{x:.1f} {y:.1f}"


def capsule(p, q, width):
    """A line from p to q with round ends, as a closed outline."""
    h = width / 2
    dx, dy = q[0] - p[0], q[1] - p[1]
    length = math.hypot(dx, dy)
    nx, ny = -dy / length * h, dx / length * h
    return (f"M{fmt(p[0] + nx, p[1] + ny)} L{fmt(q[0] + nx, q[1] + ny)} "
            f"A{h} {h} 0 0 0 {fmt(q[0] - nx, q[1] - ny)} L{fmt(p[0] - nx, p[1] - ny)} "
            f"A{h} {h} 0 0 0 {fmt(p[0] + nx, p[1] + ny)} Z")


def arc(centre, radius, width, start, end):
    """An arc with round ends from `start` to `end` degrees (clockwise, 0 = right)."""
    h = width / 2
    outer, inner = radius + h, radius - h

    def at(r, degrees):
        a = math.radians(degrees)
        return fmt(centre[0] + r * math.cos(a), centre[1] + r * math.sin(a))

    large = 1 if abs(end - start) > 180 else 0
    return (f"M{at(outer, start)} A{outer} {outer} 0 {large} 1 {at(outer, end)} "
            f"A{h} {h} 0 0 1 {at(inner, end)} A{inner} {inner} 0 {large} 0 {at(inner, start)} "
            f"A{h} {h} 0 0 1 {at(outer, start)} Z")


def circle(centre, radius):
    x, y = centre
    return f"M{fmt(x - radius, y)} A{radius} {radius} 0 1 0 {fmt(x + radius, y)} A{radius} {radius} 0 1 0 {fmt(x - radius, y)} Z"


def svg(paths, colour):
    # One <path> per shape: overlapping outlines in one path could cancel out
    # under the nonzero rule.
    body = "".join(f'<path d="{d}" fill="{colour}"/>' for d in paths)
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{body}</svg>\n'


# --- the drawings, on Icon Composer's 1024-point canvas ---

def client_layers():
    # The hood is the top of a circle around the eyes; the whole face spans
    # 274..750 vertically, centred on the canvas.
    c = (512, 562)
    return [  # top first
        ("eyes", [circle((418, 572), 60), circle((606, 572), 60)], EMBER),
        ("mouth", [capsule((476, 722), (548, 722), 56)], CREAM),
        ("hood", [arc(c, 250, 76, 185, 355)], CREAM),
    ]


def server_layers():
    left, ridge, right = (262, 542), (512, 322), (762, 542)
    return [
        [  # the eyes, in front
            ("eyes", [circle((418, 672), 66), circle((606, 672), 66)], EMBER),
        ],
        [  # the roof behind
            ("roof", [capsule(left, ridge, 72), capsule(ridge, right, 72)], CREAM),
        ],
    ]


def group(layers):
    return {
        "layers": [{"image-name": f"{name}.svg", "name": name} for name, _, _ in layers],
        "shadow": {"kind": "neutral", "opacity": 0.5},
        "translucency": {"enabled": True, "value": 0.35},
    }


def write_document(path, groups, top, bottom, dark_top, dark_bottom):
    if path.exists():
        shutil.rmtree(path)
    (path / "Assets").mkdir(parents=True)
    for layers in groups:
        for name, paths, colour in layers:
            (path / "Assets" / f"{name}.svg").write_text(svg(paths, colour))
    document = {
        "fill-specializations": [
            {"value": {"linear-gradient": [srgb(top), srgb(bottom)]}},
            {"appearance": "dark", "value": {"linear-gradient": [srgb(dark_top), srgb(dark_bottom)]}},
        ],
        "groups": [group(layers) for layers in groups],
        "supported-platforms": {"squares": ["iOS", "macOS"]},
    }
    (path / "icon.json").write_text(json.dumps(document, indent=2) + "\n")
    print(path.relative_to(ROOT))


def render(document, out, rendition="Default", size=1024, platform="macOS"):
    subprocess.run([str(ICTOOL), str(document), "--export-image", "--output-file", str(out), "--platform", platform,
                    "--rendition", rendition, "--width", str(size), "--height", str(size), "--scale", "1"],
                   check=True, stdout=subprocess.DEVNULL)


def export():
    """PNGs of both icons in target/icons, and the server's Windows icon (apps/server/icon/domovoi-server.ico)."""
    from PIL import Image

    if not ICTOOL.exists():
        sys.exit(f"needs Xcode 26's ictool at {ICTOOL}")
    out = ROOT / "target/icons"
    out.mkdir(parents=True, exist_ok=True)
    for name, document in [("domovoi", CLIENT), ("domovoi-server", SERVER)]:
        for rendition in ["Default", "Dark", "TintedDark", "ClearLight"]:
            render(document, out / f"{name}-{rendition.lower()}.png", rendition)
    # The Windows icon: the full-bleed (iOS) rendering with rounded corners and
    # a hairline margin. The macOS rendering's wide margin leaves too little
    # at 16 pixels.
    from PIL import ImageDraw

    square = out / "domovoi-server-square.png"
    render(SERVER, square, platform="iOS")
    n = 1024
    art = Image.open(square).convert("RGBA")
    inset = round(n * 0.03)
    art = art.resize((n - 2 * inset, n - 2 * inset), Image.LANCZOS)
    mask = Image.new("L", art.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, art.width - 1, art.height - 1], radius=round(art.width * 0.22), fill=255)
    rounded = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    rounded.paste(art, (inset, inset), mask)
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    rounded.resize((256, 256), Image.LANCZOS).save(EXPORTS / "domovoi-server.ico", sizes=[(s, s) for s in sizes])
    print(f"{(EXPORTS / 'domovoi-server.ico').relative_to(ROOT)}; PNG renders in {out.relative_to(ROOT)}")


def main():
    write_document(CLIENT, [client_layers()], "#2B3157", "#11142A", "#1B1F38", "#07080F")
    write_document(SERVER, server_layers(), "#1E2238", "#090B17", "#151829", "#040509")
    if sys.argv[1:] == ["export"]:
        export()


if __name__ == "__main__":
    main()
