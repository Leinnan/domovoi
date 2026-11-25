#!/usr/bin/env python3
"""Draws Domovoi's icons from one geometry definition and exports platform assets.

    python3 tools/icons.py            # writes Apple .icon documents and Android vector drawables
    python3 tools/icons.py export     # also renders PNGs and the Windows .ico (needs Xcode 26 and Pillow)
    python3 tools/icons.py preview    # concept sheet of all renditions and sizes (needs rsvg-convert and Pillow)

Both icons embody Domovoi ("Night Watch Brass") — the Slavic keeper of the
hearth expressed through warm architectural craft rather than a literal face:
brushed antique champagne-brass framing a luminous hearth flame against deep
Hearth night-indigo.

- Domovoi (Mac, iPhone, iPad, Android), apps/domovoi-apple/Common/AppIcon.icon:
  a tapered carriage keeper's lantern with a warm glowing hearth flame — the
  light you carry with you.
- Domovoi Server, apps/server/icon/AppIcon.icon: a gabled hearth cottage with a
  chimney, grounded plinth and glowing arched lantern window — the home where
  the agents run.

The lantern frame, bail ring and flame also drive the Android adaptive,
monochrome and notification drawables, the status bar glyph
(apps/server/src/tray/icon.rs) and the in-app badge
(apps/domovoi-apple/Common/DomovoiMark.swift); carry geometry changes across by
hand. Every shape is a filled outline rather than a stroke: Icon Composer
recolours layers for dark and tinted icons by filling their paths, which turns
an open stroked arc into a filled segment.

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

# --- Night Watch Brass palette (harmonizes with the app's Hearth UI theme) ---

CLIENT_BG = ("#262D45", "#111523")
CLIENT_BG_DARK = ("#171C2C", "#080A12")
SERVER_BG = ("#1D2235", "#0C0F19")
SERVER_BG_DARK = ("#121622", "#06080E")

BRASS_STOPS = [(0.0, "#F3DFB4"), (0.45, "#D6B070"), (1.0, "#9A7038")]
BRASS_RING_STOPS = [(0.0, "#F7E7C4"), (1.0, "#BA9152")]
CHAMBER_STOPS = [(0.0, "#FFE08A"), (0.50, "#EC8C22"), (1.0, "#9C3F10")]
FLAME_STOPS = [(0.0, "#FFFDF6"), (0.56, "#FFEDA8"), (1.0, "#FFBC4C")]
GLOW_STOPS = [(0.0, "#FFA838", 0.34), (0.40, "#E87A1E", 0.1428), (0.76, "#9E3B10", 0.034), (1.0, "#000000", 0.0)]


def srgb(hex_colour, alpha=1.0):
    r, g, b = (int(hex_colour[i:i + 2], 16) / 255 for i in (1, 3, 5))
    return f"srgb:{r:.5f},{g:.5f},{b:.5f},{alpha:.5f}"


def svg_wrap(body, defs=""):
    d = f"<defs>{defs}</defs>" if defs else ""
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{d}{body}</svg>\n'


def _svg_stops(stops):
    parts = []
    for stop in stops:
        off, colour = stop[0], stop[1]
        op = stop[2] if len(stop) > 2 else None
        extra = f' stop-opacity="{op:.4f}"' if op is not None else ""
        parts.append(f'<stop offset="{off * 100:.0f}%" stop-color="{colour}"{extra}/>')
    return "".join(parts)


def linear(grad_id, y0, y1, stops):
    return (
        f'<linearGradient id="{grad_id}" x1="512" y1="{y0}" x2="512" y2="{y1}" gradientUnits="userSpaceOnUse">'
        f"{_svg_stops(stops)}</linearGradient>"
    )


def radial(grad_id, cx, cy, r, stops):
    return (
        f'<radialGradient id="{grad_id}" cx="{cx}" cy="{cy}" r="{r}" gradientUnits="userSpaceOnUse">'
        f"{_svg_stops(stops)}</radialGradient>"
    )


# --- Path helpers (absolute M/C/L/Z only, so transform_path can map them) ---

K = 0.55228475


def circle_path(cx, cy, r):
    k = r * K
    return (
        f"M {cx:.1f} {cy - r:.1f} "
        f"C {cx + k:.1f} {cy - r:.1f}, {cx + r:.1f} {cy - k:.1f}, {cx + r:.1f} {cy:.1f} "
        f"C {cx + r:.1f} {cy + k:.1f}, {cx + k:.1f} {cy + r:.1f}, {cx:.1f} {cy + r:.1f} "
        f"C {cx - k:.1f} {cy + r:.1f}, {cx - r:.1f} {cy + k:.1f}, {cx - r:.1f} {cy:.1f} "
        f"C {cx - r:.1f} {cy - k:.1f}, {cx - k:.1f} {cy - r:.1f}, {cx:.1f} {cy - r:.1f} Z"
    )


def rounded_polygon(points, radius):
    """A closed polygon with each corner rounded by `radius` (a number or one per point)."""
    radii = radius if isinstance(radius, (list, tuple)) else [radius] * len(points)
    n = len(points)
    segs = []
    for i, (px, py) in enumerate(points):
        ax, ay = points[i - 1]
        bx, by = points[(i + 1) % n]
        r = radii[i]
        da = math.hypot(ax - px, ay - py)
        db = math.hypot(bx - px, by - py)
        p1 = (px + (ax - px) / da * r, py + (ay - py) / da * r)
        p2 = (px + (bx - px) / db * r, py + (by - py) / db * r)
        c1 = (p1[0] + (px - p1[0]) * K, p1[1] + (py - p1[1]) * K)
        c2 = (p2[0] + (px - p2[0]) * K, p2[1] + (py - p2[1]) * K)
        segs.append((p1, c1, c2, p2))
    out = [f"M {segs[0][0][0]:.1f} {segs[0][0][1]:.1f}"]
    for i, (p1, c1, c2, p2) in enumerate(segs):
        if i:
            out.append(f"L {p1[0]:.1f} {p1[1]:.1f}")
        out.append(f"C {c1[0]:.1f} {c1[1]:.1f}, {c2[0]:.1f} {c2[1]:.1f}, {p2[0]:.1f} {p2[1]:.1f}")
    return " ".join(out) + " Z"


def arch_path(cx, top_y, bottom_y, half_w, arch_r, bottom_r=26):
    """Semicircular-topped architectural arch with softly rounded base corners."""
    x0, x1 = cx - half_w, cx + half_w
    sp_y = top_y + arch_r
    k = arch_r * K
    bk = bottom_r * K
    return (
        f"M {cx:.1f} {top_y:.1f} "
        f"C {cx + k:.1f} {top_y:.1f}, {x1:.1f} {sp_y - k:.1f}, {x1:.1f} {sp_y:.1f} "
        f"L {x1:.1f} {bottom_y - bottom_r:.1f} "
        f"C {x1:.1f} {bottom_y - bottom_r + bk:.1f}, {x1 - bottom_r + bk:.1f} {bottom_y:.1f}, {x1 - bottom_r:.1f} {bottom_y:.1f} "
        f"L {x0 + bottom_r:.1f} {bottom_y:.1f} "
        f"C {x0 + bottom_r - bk:.1f} {bottom_y:.1f}, {x0:.1f} {bottom_y - bottom_r + bk:.1f}, {x0:.1f} {bottom_y - bottom_r:.1f} "
        f"L {x0:.1f} {sp_y:.1f} "
        f"C {x0:.1f} {sp_y - k:.1f}, {cx - k:.1f} {top_y:.1f}, {cx:.1f} {top_y:.1f} Z"
    )


def tapered_window_path(cx, top_y, bottom_y, hw_top, hw_bot, arch_r, bottom_r=24):
    """Arched lantern glass window tapering subtly inward toward the plinth."""
    sp_y = top_y + arch_r
    k = arch_r * K
    bk = bottom_r * K
    x0t, x1t = cx - hw_top, cx + hw_top
    x0b, x1b = cx - hw_bot, cx + hw_bot
    return (
        f"M {cx:.1f} {top_y:.1f} "
        f"C {cx + k:.1f} {top_y:.1f}, {x1t:.1f} {sp_y - k:.1f}, {x1t:.1f} {sp_y:.1f} "
        f"L {x1b:.1f} {bottom_y - bottom_r:.1f} "
        f"C {x1b:.1f} {bottom_y - bottom_r + bk:.1f}, {x1b - bottom_r + bk:.1f} {bottom_y:.1f}, {x1b - bottom_r:.1f} {bottom_y:.1f} "
        f"L {x0b + bottom_r:.1f} {bottom_y:.1f} "
        f"C {x0b + bottom_r - bk:.1f} {bottom_y:.1f}, {x0b:.1f} {bottom_y - bottom_r + bk:.1f}, {x0b:.1f} {bottom_y - bottom_r:.1f} "
        f"L {x0t:.1f} {sp_y:.1f} "
        f"C {x0t:.1f} {sp_y - k:.1f}, {cx - k:.1f} {top_y:.1f}, {cx:.1f} {top_y:.1f} Z"
    )


def flame_path(cx, top_y, bottom_y, half_w, waist_frac=0.63):
    """A poised, symmetrical hearth flame: crisp tip, warm belly, softly rounded base."""
    h = bottom_y - top_y
    waist_y = top_y + h * waist_frac
    return (
        f"M {cx:.1f} {top_y:.1f} "
        f"C {cx + half_w * 0.24:.1f} {top_y + h * 0.20:.1f}, {cx + half_w:.1f} {waist_y - h * 0.23:.1f}, {cx + half_w:.1f} {waist_y:.1f} "
        f"C {cx + half_w:.1f} {bottom_y - h * 0.05:.1f}, {cx + half_w * 0.54:.1f} {bottom_y:.1f}, {cx:.1f} {bottom_y:.1f} "
        f"C {cx - half_w * 0.54:.1f} {bottom_y:.1f}, {cx - half_w:.1f} {bottom_y - h * 0.05:.1f}, {cx - half_w:.1f} {waist_y:.1f} "
        f"C {cx - half_w:.1f} {waist_y - h * 0.23:.1f}, {cx - half_w * 0.24:.1f} {top_y + h * 0.20:.1f}, {cx:.1f} {top_y:.1f} Z"
    )


# --- Canonical 1024-point geometry: Client (Tapered Keeper's Lantern) ---

CLIENT_BODY_POINTS = [
    (512, 224),   # dome peak
    (782, 344),   # right hood flare
    (742, 370),   # right shoulder tuck
    (706, 756),   # tapered right lower wall
    (748, 778),   # right plinth flare
    (748, 838),   # right plinth bottom
    (276, 838),   # left plinth bottom
    (276, 778),   # left plinth flare
    (318, 756),   # tapered left lower wall
    (282, 370),   # left shoulder tuck
    (242, 344),   # left hood flare
]
CLIENT_BODY_RADII = [60, 22, 12, 12, 18, 28, 28, 18, 12, 12, 22]

CLIENT_OUTER_PATH = rounded_polygon(CLIENT_BODY_POINTS, CLIENT_BODY_RADII)
CLIENT_WINDOW_PATH = tapered_window_path(512, 396, 744, hw_top=162, hw_bot=138, arch_r=148, bottom_r=24)
# Slightly larger than the window cutout so the glass tucks seamlessly under the brass bezel.
CLIENT_CHAMBER_PATH = tapered_window_path(512, 392, 748, hw_top=166, hw_bot=142, arch_r=152, bottom_r=26)
CLIENT_RING_OUTER = circle_path(512, 186, 56)
CLIENT_RING_INNER = circle_path(512, 186, 22)
CLIENT_FLAME_PATH = flame_path(512, 470, 710, half_w=82, waist_frac=0.63)

# Slightly wider window cutout for crisp 16–22px monochrome tray and notification glyphs.
MONO_WINDOW_PATH = tapered_window_path(512, 394, 744, hw_top=172, hw_bot=148, arch_r=154, bottom_r=24)
MONO_FLAME_PATH = flame_path(512, 476, 710, half_w=76, waist_frac=0.63)

# --- Canonical 1024-point geometry: Server (Gabled Hearth Cottage) ---

SERVER_BODY_POINTS = [
    (512, 180),   # roof peak
    (652, 306),   # right roof meets chimney
    (652, 224),   # chimney top-left
    (736, 224),   # chimney top-right
    (736, 382),   # chimney meets right slope
    (852, 486),   # right eave tip
    (768, 486),   # right eave underside
    (768, 764),   # right wall base
    (802, 782),   # right plinth flare
    (802, 836),   # right plinth bottom
    (222, 836),   # left plinth bottom
    (222, 782),   # left plinth flare
    (256, 764),   # left wall base
    (256, 486),   # left eave underside
    (172, 486),   # left eave tip
]
SERVER_BODY_RADII = [44, 10, 18, 18, 10, 26, 12, 10, 18, 28, 28, 18, 10, 12, 26]

SERVER_OUTER_PATH = rounded_polygon(SERVER_BODY_POINTS, SERVER_BODY_RADII)
SERVER_WINDOW_PATH = arch_path(512, 476, 766, half_w=142, arch_r=142, bottom_r=26)
# Slightly larger than the window cutout so the glass tucks seamlessly under the brass bezel.
SERVER_CHAMBER_PATH = arch_path(512, 472, 770, half_w=146, arch_r=146, bottom_r=28)
SERVER_FLAME_PATH = flame_path(512, 538, 736, half_w=74, waist_frac=0.63)


# --- Layer SVGs ---

def glow_svg(cx, cy, r_outer):
    """Full-bleed 1024x1024 radial hearth bloom fading to 0 opacity before the edge
    so Icon Composer never draws a glass rim around the glow inside the icon."""
    return svg_wrap(
        '<rect width="1024" height="1024" fill="url(#ambGlow)"/>',
        defs=radial("ambGlow", cx, cy, r_outer, GLOW_STOPS),
    )


def client_layers():
    """Layer SVGs, top to bottom."""
    frame = (
        f'<path d="{CLIENT_RING_OUTER} {CLIENT_RING_INNER}" fill="url(#ringGrad)" fill-rule="evenodd"/>'
        f'<path d="{CLIENT_OUTER_PATH} {CLIENT_WINDOW_PATH}" fill="url(#brassGrad)" fill-rule="evenodd"/>'
    )
    return {
        "frame": svg_wrap(frame, defs=linear("brassGrad", 180, 838, BRASS_STOPS) + linear("ringGrad", 130, 242, BRASS_RING_STOPS)),
        "flame": svg_wrap(f'<path d="{CLIENT_FLAME_PATH}" fill="url(#flameGrad)"/>', defs=linear("flameGrad", 470, 710, FLAME_STOPS)),
        "chamber": svg_wrap(f'<path d="{CLIENT_CHAMBER_PATH}" fill="url(#chamberGrad)"/>', defs=radial("chamberGrad", 512, 600, 210, CHAMBER_STOPS)),
        "glow": glow_svg(512, 580, 440),
    }


def server_layers():
    """Layer SVGs, top to bottom."""
    frame = f'<path d="{SERVER_OUTER_PATH} {SERVER_WINDOW_PATH}" fill="url(#brassGrad)" fill-rule="evenodd"/>'
    return {
        "frame": svg_wrap(frame, defs=linear("brassGrad", 180, 838, BRASS_STOPS)),
        "flame": svg_wrap(f'<path d="{SERVER_FLAME_PATH}" fill="url(#flameGrad)"/>', defs=linear("flameGrad", 538, 736, FLAME_STOPS)),
        "chamber": svg_wrap(f'<path d="{SERVER_CHAMBER_PATH}" fill="url(#chamberGrad)"/>', defs=radial("chamberGrad", 512, 646, 190, CHAMBER_STOPS)),
        "glow": glow_svg(512, 620, 420),
    }


def mono_glyph_paths():
    """([even-odd paths], [solid paths]) for single-colour tray, monochrome and notification glyphs."""
    return [f"{CLIENT_RING_OUTER} {CLIENT_RING_INNER}", f"{CLIENT_OUTER_PATH} {MONO_WINDOW_PATH}"], [MONO_FLAME_PATH]


def mono_glyph_svg():
    """The single-colour lantern glyph as an SVG."""
    eo_paths, solids = mono_glyph_paths()
    body = "".join(f'<path fill-rule="evenodd" d="{d}"/>' for d in eo_paths) + "".join(f'<path d="{d}"/>' for d in solids)
    return svg_wrap(f'<g fill="#000">{body}</g>')


# --- Icon Composer documents ---

def build_icon_document(bg, bg_dark, frame_shadow=0.50):
    groups = [
        {
            "layers": [{"image-name": "frame.svg", "name": "frame"}],
            "shadow": {"kind": "neutral", "opacity": frame_shadow},
            "translucency": {"enabled": True, "value": 0.12},
            "specular": True,
        },
        {
            "layers": [{"image-name": "flame.svg", "name": "flame"}],
            "shadow": {"kind": "none", "opacity": 0.0},
            "specular": False,
        },
        {
            "layers": [{"image-name": "chamber.svg", "name": "chamber"}],
            "shadow": {"kind": "none", "opacity": 0.0},
            "specular": False,
        },
        {
            "layers": [
                {
                    "image-name": "glow.svg",
                    "name": "glow",
                    "hidden-specializations": [{"appearance": "tinted", "value": True}],
                }
            ],
            "glass": False,
            "specular": False,
            "shadow": {"kind": "none", "opacity": 0.0},
        },
    ]
    return {
        "fill-specializations": [
            {"value": {"linear-gradient": [srgb(bg[0]), srgb(bg[1])]}},
            {"appearance": "dark", "value": {"linear-gradient": [srgb(bg_dark[0]), srgb(bg_dark[1])]}},
        ],
        "groups": groups,
        "supported-platforms": {"squares": ["iOS", "macOS"]},
    }


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


def argb(hex_colour, alpha=1.0):
    return f"#{round(alpha * 255):02X}{hex_colour[1:].upper()}"


def android_gradient(kind, stops, **attrs):
    a = " ".join(f'android:{k}="{v:.2f}"' if isinstance(v, float) else f'android:{k}="{v}"' for k, v in attrs.items())
    items = []
    for stop in stops:
        off, colour = stop[0], stop[1]
        alpha = stop[2] if len(stop) > 2 else 1.0
        items.append(f'                <item android:color="{argb(colour, alpha)}" android:offset="{off:.2f}" />')
    items_str = "\n".join(items)
    return f"""        <aapt:attr name="android:fillColor">
            <gradient android:type="{kind}" {a}>
{items_str}
            </gradient>
        </aapt:attr>"""


def write_android_drawables():
    drawable = ANDROID_RES / "drawable"
    mipmap = ANDROID_RES / "mipmap-anydpi"

    # 1. Adaptive icon background (108x108dp): Hearth night-indigo gradient + soft amber glow.
    bg_stops = [(0.0, CLIENT_BG[0]), (1.0, CLIENT_BG[1])]
    bg_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <!-- Generated by tools/icons.py -->
    <path android:pathData="M0,0H108V108H0Z">
{android_gradient("linear", bg_stops, startX=54.0, startY=0.0, endX=54.0, endY=108.0)}
    </path>
    <path android:pathData="M0,0H108V108H0Z">
{android_gradient("radial", GLOW_STOPS, centerX=54.0, centerY=60.5, gradientRadius=33.0)}
    </path>
</vector>
"""
    (drawable / "ic_launcher_background.xml").write_text(bg_xml)

    # 2. Adaptive icon foreground, fitted cleanly inside Android's 66 dp safe circle.
    # Lantern spans y=130..838 (centre y=484, height 708); s=0.072 -> ~51 dp tall.
    s = 0.072
    dx = 54.0 - 512.0 * s
    dy = 54.0 - 484.0 * s

    def t(d):
        return transform_path(d, s, dx, dy)

    def pt(x, y):
        return x * s + dx, y * s + dy

    cx_c, cy_c = pt(512, 600)
    blocks = [
        f'    <path android:pathData="{t(CLIENT_CHAMBER_PATH)}">\n'
        + android_gradient("radial", CHAMBER_STOPS, centerX=cx_c, centerY=cy_c, gradientRadius=210 * s)
        + "\n    </path>",
        f'    <path android:pathData="{t(CLIENT_FLAME_PATH)}">\n'
        + android_gradient("linear", FLAME_STOPS, startX=54.0, startY=pt(0, 470)[1], endX=54.0, endY=pt(0, 710)[1])
        + "\n    </path>",
        f'    <path android:fillType="evenOdd" android:pathData="{t(f"{CLIENT_RING_OUTER} {CLIENT_RING_INNER}")}">\n'
        + android_gradient("linear", BRASS_RING_STOPS, startX=54.0, startY=pt(0, 130)[1], endX=54.0, endY=pt(0, 242)[1])
        + "\n    </path>",
        f'    <path android:fillType="evenOdd" android:pathData="{t(f"{CLIENT_OUTER_PATH} {CLIENT_WINDOW_PATH}")}">\n'
        + android_gradient("linear", BRASS_STOPS, startX=54.0, startY=pt(0, 180)[1], endX=54.0, endY=pt(0, 838)[1])
        + "\n    </path>",
    ]
    body = "\n".join(blocks)
    fg_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <!-- The keeper's lantern: brushed brass frame, amber chamber and hearth flame
         (generated by tools/icons.py), fitted inside Android's 66 dp adaptive safe zone. -->
{body}
</vector>
"""
    (drawable / "ic_launcher_foreground.xml").write_text(fg_xml)

    # 3 and 4. Monochrome adaptive layer and 24dp notification glyph.
    def glyph(scale, ox, oy):
        eo_paths, solids = mono_glyph_paths()
        lines = [
            f'    <path android:fillColor="#FFFFFFFF" android:fillType="evenOdd" android:pathData="{transform_path(d, scale, ox, oy)}" />'
            for d in eo_paths
        ]
        lines += [
            f'    <path android:fillColor="#FFFFFFFF" android:pathData="{transform_path(d, scale, ox, oy)}" />'
            for d in solids
        ]
        return "\n".join(lines)

    mono_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <!-- Monochrome adaptive layer (generated by tools/icons.py). -->
{glyph(s, dx, dy)}
</vector>
"""
    (drawable / "ic_launcher_monochrome.xml").write_text(mono_xml)

    ns = 0.0285
    stat_xml = f"""<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="24dp" android:height="24dp"
    android:viewportWidth="24" android:viewportHeight="24">
    <!-- Notification glyph: keeper's lantern and hearth flame (generated by tools/icons.py). -->
{glyph(ns, 12.0 - 512.0 * ns, 12.0 - 484.0 * ns)}
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
    print(ANDROID_RES.relative_to(ROOT))


# --- Rendering ---

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


def flat_composite(layers, bg):
    """A flat (no glass) preview: rounded backdrop plus the layers, bottom to top."""
    inner = []
    for svg in reversed(list(layers.values())):
        inner.append(svg.replace('xmlns="http://www.w3.org/2000/svg" ', ""))
    return svg_wrap(
        '<rect width="1024" height="1024" rx="230" fill="url(#bg)"/>' + "".join(inner),
        defs=linear("bg", 0, 1024, [(0.0, bg[0]), (1.0, bg[1])]),
    )


def rasterise(svg_text, out, size=1024):
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", str(out)], input=svg_text.encode(), check=True)


def preview():
    """target/icons/concepts.png: client, server and monochrome glyph at several sizes on light and dark."""
    from PIL import Image, ImageDraw

    out = ROOT / "target/icons/concepts"
    out.mkdir(parents=True, exist_ok=True)
    variants = []
    for label, svg_text, fname in [
        ("client (light)", flat_composite(client_layers(), CLIENT_BG), "client-light.png"),
        ("client (dark)", flat_composite(client_layers(), CLIENT_BG_DARK), "client-dark.png"),
        ("server (light)", flat_composite(server_layers(), SERVER_BG), "server-light.png"),
        ("server (dark)", flat_composite(server_layers(), SERVER_BG_DARK), "server-dark.png"),
        ("glyph", mono_glyph_svg(), "glyph.png"),
    ]:
        f = out / fname
        rasterise(svg_text, f)
        variants.append((label, f))

    sizes = [256, 128, 64, 32, 16]
    row_h = 280
    width = sum(sizes) + 20 * len(sizes) + 260
    sheet = Image.new("RGB", (width * 2, row_h * len(variants)), "white")
    draw = ImageDraw.Draw(sheet)
    for side, backdrop in enumerate(["#F2F2F4", "#1E1E22"]):
        draw.rectangle([side * width, 0, (side + 1) * width, sheet.height], fill=backdrop)
        for row, (label, f) in enumerate(variants):
            art = Image.open(f).convert("RGBA")
            x = side * width + 20
            y = row * row_h + 12
            draw.text((x, y), label, fill="#888888")
            for s in sizes:
                im = art.resize((s, s), Image.LANCZOS)
                if label == "glyph" and side == 1:
                    r, g, b, a = im.split()
                    im = Image.merge("RGBA", (r.point(lambda _: 255), g.point(lambda _: 255), b.point(lambda _: 255), a))
                sheet.paste(im, (x, y + 16), im)
                x += s + 20
            tiny = art.resize((16, 16), Image.LANCZOS)
            if label == "glyph" and side == 1:
                r, g, b, a = tiny.split()
                tiny = Image.merge("RGBA", (r.point(lambda _: 255), g.point(lambda _: 255), b.point(lambda _: 255), a))
            zoom = tiny.resize((128, 128), Image.NEAREST)
            sheet.paste(zoom, (x, y + 16), zoom)
    sheet.save(ROOT / "target/icons/concepts.png")
    print((ROOT / "target/icons/concepts.png").relative_to(ROOT))


def squared(document, name, out, n=1024):
    """The full-bleed (iOS) rendering with rounded corners and a hairline margin.
    The macOS rendering's wide margin leaves too little at 16 pixels."""
    from PIL import Image, ImageDraw

    square = out / f"{name}-square.png"
    render(document, square, platform="iOS")
    art = Image.open(square).convert("RGBA")
    inset = round(n * 0.03)
    art = art.resize((n - 2 * inset, n - 2 * inset), Image.LANCZOS)
    mask = Image.new("L", art.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, art.width - 1, art.height - 1], radius=round(art.width * 0.22), fill=255)
    rounded = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    rounded.paste(art, (inset, inset), mask)
    return rounded


def export():
    """PNGs of both icons in target/icons, the server's Windows icon and the Iced desktop icons."""
    from PIL import Image

    if not ICTOOL.exists():
        sys.exit(f"needs Xcode 26's ictool at {ICTOOL}")
    out = ROOT / "target/icons"
    out.mkdir(parents=True, exist_ok=True)
    for name, document in [("domovoi", CLIENT), ("domovoi-server", SERVER)]:
        for rendition in ["Default", "Dark", "TintedDark", "ClearLight"]:
            render(document, out / f"{name}-{rendition.lower()}.png", rendition)
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    server = squared(SERVER, "domovoi-server", out)
    server.resize((256, 256), Image.LANCZOS).save(EXPORTS / "domovoi-server.ico", sizes=[(s, s) for s in sizes])

    # Iced desktop client icons: 256px full-bleed PNG (Linux/Windows/hero),
    # 512px macOS Dock PNG, and multi-size Windows .ico.
    ICED_ASSETS.mkdir(parents=True, exist_ok=True)
    client = squared(CLIENT, "domovoi-client", out)
    client.resize((256, 256), Image.LANCZOS).save(ICED_ASSETS / "app-icon-256.png")
    client.resize((256, 256), Image.LANCZOS).save(ICED_ASSETS / "app-icon.ico", sizes=[(s, s) for s in sizes])
    Image.open(out / "domovoi-default.png").convert("RGBA").resize((512, 512), Image.LANCZOS).save(
        ICED_ASSETS / "app-icon-macos.png"
    )
    print(
        f"{(EXPORTS / 'domovoi-server.ico').relative_to(ROOT)}; "
        f"Iced icons in {ICED_ASSETS.relative_to(ROOT)}; PNG renders in {out.relative_to(ROOT)}"
    )


def main():
    if sys.argv[1:] == ["preview"]:
        preview()
        return
    write_document(CLIENT, build_icon_document(CLIENT_BG, CLIENT_BG_DARK, frame_shadow=0.50), client_layers())
    write_document(SERVER, build_icon_document(SERVER_BG, SERVER_BG_DARK, frame_shadow=0.52), server_layers())
    write_android_drawables()
    if sys.argv[1:] == ["export"]:
        export()


if __name__ == "__main__":
    main()
