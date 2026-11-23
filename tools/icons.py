#!/usr/bin/env python3
"""Draws Domovoi's icons from one geometry definition and exports platform assets.

    python3 tools/icons.py            # writes Apple .icon documents and Android vector drawables
    python3 tools/icons.py export     # also renders PNGs and the Windows .ico (needs Xcode 26 and Pillow)

Two icons share one look, the domovoi (the Slavic house spirit) watching from
the dark: two luminous ember eyes under shelter against night indigo.

- Domovoi (Mac, iPhone, iPad, Android), apps/domovoi-apple/Common/AppIcon.icon:
  a sculpted, gently peaked cloak-hood over glowing ember eyes — the spirit
  you carry with you.
- Domovoi Server, apps/server/icon/AppIcon.icon: a crisp gabled house roof
  with a chimney over the same glowing eyes — the spirit at home, where the
  agents run.

The peaked hood and eyes also drive the Android adaptive/monochrome/notification
drawables and match the status bar glyph (apps/server/src/tray/icon.rs). Every
shape is a filled outline rather than a stroke: Icon Composer recolours layers
for dark and tinted icons by filling their paths, which turns an open stroked
arc into a filled segment.

The .icon documents open in Icon Composer (Xcode > Open Developer Tool) for
tuning the glass, shadows and appearances; rerunning this script overwrites
them, so carry lasting changes back to the numbers here.
"""
import json
import math
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLIENT = ROOT / "apps/domovoi-apple/Common/AppIcon.icon"
SERVER = ROOT / "apps/server/icon/AppIcon.icon"
EXPORTS = ROOT / "apps/server/icon"
ICED_ASSETS = ROOT / "apps/domovoi-iced/assets"
ANDROID_RES = ROOT / "apps/domovoi-android/app/src/main/res"
ICTOOL = Path("/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool")

# Shared palette
CREAM_TOP = "#FFF8EB"
CREAM_MID = "#F9E9CC"
CREAM_BOT = "#E5CEA6"
EMBER_CORE = "#FFF4B8"
EMBER_MID = "#FFC23D"
EMBER_WARM = "#F28A16"
EMBER_EDGE = "#CF5E06"


def srgb(hex_colour, alpha=1.0):
    r, g, b = (int(hex_colour[i:i + 2], 16) / 255 for i in (1, 3, 5))
    return f"srgb:{r:.5f},{g:.5f},{b:.5f},{alpha:.5f}"


def svg_wrap(body, defs=""):
    d = f"<defs>{defs}</defs>" if defs else ""
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{d}{body}</svg>\n'


def squircle_eye_path(cx, cy, rx, ry, n=2.55, tilt_deg=0.0, steps=72):
    """Smooth superellipse eye path (warmer and more alive than a rigid circle)."""
    pts = []
    rad = math.radians(tilt_deg)
    ct, st = math.cos(rad), math.sin(rad)
    for i in range(steps):
        a = 2 * math.pi * i / steps
        ca, sa = math.cos(a), math.sin(a)
        x = math.copysign(abs(ca) ** (2.0 / n), ca) * rx
        y = math.copysign(abs(sa) ** (2.0 / n), sa) * ry
        xr = cx + x * ct - y * st
        yr = cy + x * st + y * ct
        pts.append(f"{xr:.1f} {yr:.1f}")
    return "M" + " L".join(pts) + " Z"


def ember_eye_defs(eye_id, cx, cy, r):
    return (
        f'<radialGradient id="{eye_id}" cx="{cx:.1f}" cy="{cy - r * 0.20:.1f}" r="{r * 1.18:.1f}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0%" stop-color="{EMBER_CORE}"/>'
        f'<stop offset="32%" stop-color="{EMBER_MID}"/>'
        f'<stop offset="76%" stop-color="{EMBER_WARM}"/>'
        f'<stop offset="100%" stop-color="{EMBER_EDGE}"/>'
        f"</radialGradient>"
    )


def cream_grad_defs(grad_id, y0, y1):
    return (
        f'<linearGradient id="{grad_id}" x1="512" y1="{y0}" x2="512" y2="{y1}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0%" stop-color="{CREAM_TOP}"/>'
        f'<stop offset="52%" stop-color="{CREAM_MID}"/>'
        f'<stop offset="100%" stop-color="{CREAM_BOT}"/>'
        f"</linearGradient>"
    )


def atmosphere_svg(cx, cy, glow_r=265.0, glow_op=0.45, dark_r=360.0):
    """Full-bleed 1024x1024 radial recess + hearth bloom.

    Full-canvas rects fade to 0 opacity before the squircle mask so Icon
    Composer never draws a glass rim around the glow inside the icon.
    """
    return svg_wrap(
        '<rect width="1024" height="1024" fill="url(#darkPool)"/>'
        '<rect width="1024" height="1024" fill="url(#hearthBloom)"/>',
        defs=(
            f'<radialGradient id="darkPool" cx="{cx:.1f}" cy="{cy:.1f}" r="{dark_r:.1f}" gradientUnits="userSpaceOnUse">'
            f'<stop offset="0%" stop-color="#050712" stop-opacity="0.72"/>'
            f'<stop offset="68%" stop-color="#070916" stop-opacity="0.32"/>'
            f'<stop offset="100%" stop-color="#070916" stop-opacity="0.0"/>'
            f"</radialGradient>"
            f'<radialGradient id="hearthBloom" cx="{cx:.1f}" cy="{cy:.1f}" r="{glow_r:.1f}" gradientUnits="userSpaceOnUse">'
            f'<stop offset="0%" stop-color="#FF9D24" stop-opacity="{glow_op:.2f}"/>'
            f'<stop offset="46%" stop-color="#E87812" stop-opacity="{glow_op * 0.38:.2f}"/>'
            f'<stop offset="100%" stop-color="#E87812" stop-opacity="0.0"/>'
            f"</radialGradient>"
        ),
    )


# --- Canonical 1024-point silhouettes ---

CLIENT_HOOD_PATH = (
    "M 512 204 "
    "C 554 204, 636 254, 718 352 "
    "C 792 440, 844 552, 858 666 "
    "C 864 708, 832 742, 792 738 "
    "C 758 734, 738 710, 730 672 "
    "C 714 596, 682 518, 634 452 "
    "C 590 392, 544 358, 512 358 "
    "C 480 358, 434 392, 390 452 "
    "C 342 518, 310 596, 294 672 "
    "C 286 710, 266 734, 232 738 "
    "C 192 742, 160 708, 166 666 "
    "C 180 552, 232 440, 306 352 "
    "C 388 254, 470 204, 512 204 Z"
)

SERVER_ROOF_PATH = (
    "M 512 224 "
    "C 528 224, 544 230, 558 242 "
    "L 652 326 "
    "L 652 276 C 652 258, 666 244, 684 244 L 724 244 C 742 244, 756 258, 756 276 L 756 418 "
    "L 848 500 "
    "C 872 522, 874 558, 852 582 "
    "C 830 606, 794 608, 770 586 "
    "L 534 376 "
    "C 522 365, 502 365, 490 376 "
    "L 254 586 "
    "C 230 608, 194 606, 172 582 "
    "C 150 558, 152 522, 176 500 "
    "L 466 242 "
    "C 480 230, 496 224, 512 224 Z"
)

CLIENT_EYES = [(404, 622, 60, 74, -2.5), (620, 622, 60, 74, 2.5)]
SERVER_EYES = [(404, 666, 62, 74, -1.5), (620, 666, 62, 74, 1.5)]


def eyes_svg(specs, prefix):
    paths = []
    defs = []
    for idx, (cx, cy, rx, ry, tilt) in enumerate(specs):
        gid = f"{prefix}{idx}"
        paths.append(f'<path d="{squircle_eye_path(cx, cy, rx, ry, tilt_deg=tilt)}" fill="url(#{gid})"/>')
        defs.append(ember_eye_defs(gid, cx, cy, ry))
    return svg_wrap("".join(paths), defs="".join(defs))


def build_icon_document(primary_name, primary_svg, eye_specs, eye_prefix, glow_cy, top, bottom, dark_top, dark_bottom):
    groups = [
        {
            "layers": [{"image-name": f"{primary_name}.svg", "name": primary_name}],
            "shadow": {"kind": "neutral", "opacity": 0.55},
            "translucency": {"enabled": True, "value": 0.22},
            "specular": True,
        },
        {
            "layers": [{"image-name": "eyes.svg", "name": "eyes"}],
            "shadow": {"kind": "layer-color", "opacity": 0.65},
            "translucency": {"enabled": True, "value": 0.08},
            "specular": True,
        },
        {
            "layers": [
                {
                    "image-name": "atmosphere.svg",
                    "name": "atmosphere",
                    "hidden-specializations": [{"appearance": "tinted", "value": True}],
                }
            ],
            "glass": False,
            "specular": False,
            "shadow": {"kind": "none", "opacity": 0.0},
        },
    ]
    document = {
        "fill-specializations": [
            {"value": {"linear-gradient": [srgb(top), srgb(bottom)]}},
            {"appearance": "dark", "value": {"linear-gradient": [srgb(dark_top), srgb(dark_bottom)]}},
        ],
        "groups": groups,
        "supported-platforms": {"squares": ["iOS", "macOS"]},
    }
    svgs = {
        primary_name: primary_svg,
        "eyes": eyes_svg(eye_specs, eye_prefix),
        "atmosphere": atmosphere_svg(512, glow_cy, glow_r=265 if primary_name == "hood" else 275),
    }
    return document, svgs


def write_document(path, document, svgs):
    if path.exists():
        shutil.rmtree(path)
    (path / "Assets").mkdir(parents=True)
    for name, content in svgs.items():
        (path / "Assets" / f"{name}.svg").write_text(content)
    (path / "icon.json").write_text(json.dumps(document, indent=2) + "\n")
    print(path.relative_to(ROOT))


# --- Android vector drawable generation (from the same canonical paths) ---

_TOKEN_RE = re.compile(r"[MCLZ]|-?\d+(?:\.\d+)?")


def transform_path(path_d, scale, dx, dy, precision=2):
    """Affine-transforms an absolute M/C/L/Z SVG path into another coordinate space."""
    tokens = _TOKEN_RE.findall(path_d)
    out = []
    i = 0
    fmt = f"{{:.{precision}f}}"
    while i < len(tokens):
        tok = tokens[i]
        if tok in ("M", "L", "C", "Z"):
            out.append(tok)
            i += 1
        else:
            x = float(tok) * scale + dx
            y = float(tokens[i + 1]) * scale + dy
            out.append(f"{fmt.format(x)},{fmt.format(y)}")
            i += 2
    return " ".join(out)


def write_android_drawables():
    drawable = ANDROID_RES / "drawable"
    mipmap = ANDROID_RES / "mipmap-anydpi"

    # 1. Adaptive icon background (108x108dp night indigo + warm hearth bloom)
    bg_xml = """<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <!-- Generated by tools/icons.py -->
    <path android:pathData="M0,0H108V108H0Z">
        <aapt:attr name="android:fillColor">
            <gradient android:type="linear"
                android:startX="54" android:startY="0"
                android:endX="54" android:endY="108">
                <item android:color="#FF272F5B" android:offset="0.0" />
                <item android:color="#FF0D1025" android:offset="1.0" />
            </gradient>
        </aapt:attr>
    </path>
    <path android:pathData="M0,0H108V108H0Z">
        <aapt:attr name="android:fillColor">
            <gradient android:type="radial"
                android:centerX="54" android:centerY="63.9"
                android:gradientRadius="26">
                <item android:color="#B8050712" android:offset="0.0" />
                <item android:color="#52070916" android:offset="0.68" />
                <item android:color="#00070916" android:offset="1.0" />
            </gradient>
        </aapt:attr>
    </path>
    <path android:pathData="M0,0H108V108H0Z">
        <aapt:attr name="android:fillColor">
            <gradient android:type="radial"
                android:centerX="54" android:centerY="63.9"
                android:gradientRadius="19.5">
                <item android:color="#73FF9D24" android:offset="0.0" />
                <item android:color="#2BE87812" android:offset="0.46" />
                <item android:color="#00E87812" android:offset="1.0" />
            </gradient>
        </aapt:attr>
    </path>
</vector>
"""
    (drawable / "ic_launcher_background.xml").write_text(bg_xml)

    # 2. Adaptive icon foreground (scaled to 0.072 inside the 66dp safe circle)
    s = 0.072
    dx = 54.0 - 512.0 * s
    dy = 54.0 - 485.0 * s
    hood_fg = transform_path(CLIENT_HOOD_PATH, s, dx, dy)
    eye_fg_paths = [
        transform_path(squircle_eye_path(cx, cy, rx, ry, tilt_deg=tilt, steps=48), s, dx, dy)
        for cx, cy, rx, ry, tilt in CLIENT_EYES
    ]
    hood_y0 = 204.0 * s + dy
    hood_y1 = 742.0 * s + dy
    eye_blocks = []
    for (cx, cy, rx, ry, _), pdata in zip(CLIENT_EYES, eye_fg_paths):
        ecx = cx * s + dx
        ecy = (cy - ry * 0.20) * s + dy
        er = ry * 1.18 * s
        eye_blocks.append(f"""    <path android:pathData="{pdata}">
        <aapt:attr name="android:fillColor">
            <gradient android:type="radial"
                android:centerX="{ecx:.2f}" android:centerY="{ecy:.2f}"
                android:gradientRadius="{er:.2f}">
                <item android:color="#FFFFF4B8" android:offset="0.0" />
                <item android:color="#FFFFC23D" android:offset="0.32" />
                <item android:color="#FFF28A16" android:offset="0.76" />
                <item android:color="#FFCF5E06" android:offset="1.0" />
            </gradient>
        </aapt:attr>
    </path>""")

    fg_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <!-- The domovoi: sculpted Hearth Hood over two ember eyes (generated by tools/icons.py),
         fitted inside Android's 66 dp adaptive safe zone. -->
    <path android:pathData="{hood_fg}">
        <aapt:attr name="android:fillColor">
            <gradient android:type="linear"
                android:startX="54" android:startY="{hood_y0:.2f}"
                android:endX="54" android:endY="{hood_y1:.2f}">
                <item android:color="#FFFFF8EB" android:offset="0.0" />
                <item android:color="#FFF9E9CC" android:offset="0.52" />
                <item android:color="#FFE5CEA6" android:offset="1.0" />
            </gradient>
        </aapt:attr>
    </path>
{chr(10).join(eye_blocks)}
</vector>
"""
    (drawable / "ic_launcher_foreground.xml").write_text(fg_xml)

    # 3. Dedicated Material You monochrome launcher layer
    mono_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <!-- Monochrome adaptive layer (generated by tools/icons.py). -->
    <path android:fillColor="#FFFFFFFF" android:pathData="{hood_fg}" />
    <path android:fillColor="#FFFFFFFF" android:pathData="{eye_fg_paths[0]}" />
    <path android:fillColor="#FFFFFFFF" android:pathData="{eye_fg_paths[1]}" />
</vector>
"""
    (drawable / "ic_launcher_monochrome.xml").write_text(mono_xml)

    # 4. 24dp monochrome notification status icon
    ns = 0.0285
    ndx = 12.0 - 512.0 * ns
    ndy = 12.0 - 472.0 * ns
    hood_stat = transform_path(CLIENT_HOOD_PATH, ns, ndx, ndy)
    eye_stat = [
        transform_path(squircle_eye_path(cx, cy, rx, ry, tilt_deg=tilt, steps=36), ns, ndx, ndy)
        for cx, cy, rx, ry, tilt in CLIENT_EYES
    ]
    stat_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="24dp" android:height="24dp"
    android:viewportWidth="24" android:viewportHeight="24">
    <!-- Notification glyph: peaked Hearth Hood and eyes (generated by tools/icons.py). -->
    <path android:fillColor="#FFFFFFFF" android:pathData="{hood_stat}" />
    <path android:fillColor="#FFFFFFFF" android:pathData="{eye_stat[0]}" />
    <path android:fillColor="#FFFFFFFF" android:pathData="{eye_stat[1]}" />
</vector>
"""
    (drawable / "ic_stat_domovoi.xml").write_text(stat_xml)

    # 5. Adaptive icon manifest
    launcher_xml = """<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@drawable/ic_launcher_background" />
    <foreground android:drawable="@drawable/ic_launcher_foreground" />
    <monochrome android:drawable="@drawable/ic_launcher_monochrome" />
</adaptive-icon>
"""
    (mipmap / "ic_launcher.xml").write_text(launcher_xml)
    print((ANDROID_RES).relative_to(ROOT))


def render(document, out, rendition="Default", size=1024, platform="macOS"):
    subprocess.run(
        [
            str(ICTOOL), str(document), "--export-image", "--output-file", str(out),
            "--platform", platform, "--rendition", rendition,
            "--width", str(size), "--height", str(size), "--scale", "1",
        ],
        check=True,
        stdout=subprocess.DEVNULL,
    )


def export():
    """PNGs of both icons in target/icons, and the server's Windows icon (apps/server/icon/domovoi-server.ico)."""
    from PIL import Image, ImageDraw

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
    square = out / "domovoi-server-square.png"
    render(SERVER, square, platform="iOS")
    n = 1024
    art = Image.open(square).convert("RGBA")
    inset = round(n * 0.03)
    art = art.resize((n - 2 * inset, n - 2 * inset), Image.LANCZOS)
    mask = Image.new("L", art.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, art.width - 1, art.height - 1], radius=round(art.width * 0.22), fill=255
    )
    rounded = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    rounded.paste(art, (inset, inset), mask)
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    rounded.resize((256, 256), Image.LANCZOS).save(EXPORTS / "domovoi-server.ico", sizes=[(s, s) for s in sizes])

    # Iced desktop client icons: 256px full-bleed PNG (Linux/Windows/hero),
    # 512px macOS Dock PNG, and multi-size Windows .ico.
    ICED_ASSETS.mkdir(parents=True, exist_ok=True)
    client_square = out / "domovoi-client-square.png"
    render(CLIENT, client_square, platform="iOS")
    client_art = Image.open(client_square).convert("RGBA")
    client_inset = round(n * 0.03)
    client_resized = client_art.resize((n - 2 * client_inset, n - 2 * client_inset), Image.LANCZOS)
    client_mask = Image.new("L", client_resized.size, 0)
    ImageDraw.Draw(client_mask).rounded_rectangle(
        [0, 0, client_resized.width - 1, client_resized.height - 1],
        radius=round(client_resized.width * 0.22),
        fill=255,
    )
    client_rounded = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    client_rounded.paste(client_resized, (client_inset, client_inset), client_mask)
    client_rounded.resize((256, 256), Image.LANCZOS).save(ICED_ASSETS / "app-icon-256.png")
    client_rounded.resize((256, 256), Image.LANCZOS).save(ICED_ASSETS / "app-icon.ico", sizes=[(s, s) for s in sizes])
    Image.open(out / "domovoi-default.png").convert("RGBA").resize((512, 512), Image.LANCZOS).save(
        ICED_ASSETS / "app-icon-macos.png"
    )
    print(
        f"{(EXPORTS / 'domovoi-server.ico').relative_to(ROOT)}; "
        f"Iced icons in {ICED_ASSETS.relative_to(ROOT)}; PNG renders in {out.relative_to(ROOT)}"
    )


def main():
    client_hood = svg_wrap(f'<path d="{CLIENT_HOOD_PATH}" fill="url(#hoodGrad)"/>', defs=cream_grad_defs("hoodGrad", 204, 742))
    client_doc, client_svgs = build_icon_document(
        "hood", client_hood, CLIENT_EYES, "e", 622, "#272F5B", "#0D1025", "#181D38", "#050710"
    )
    write_document(CLIENT, client_doc, client_svgs)

    server_roof = svg_wrap(f'<path d="{SERVER_ROOF_PATH}" fill="url(#roofGrad)"/>', defs=cream_grad_defs("roofGrad", 224, 608))
    server_doc, server_svgs = build_icon_document(
        "roof", server_roof, SERVER_EYES, "se", 666, "#1D223E", "#080A16", "#131628", "#04050A"
    )
    write_document(SERVER, server_doc, server_svgs)

    write_android_drawables()

    if sys.argv[1:] == ["export"]:
        export()


if __name__ == "__main__":
    main()
