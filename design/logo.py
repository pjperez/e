#!/usr/bin/env python3
"""
e — brand mark generator.

The mark is built from the one place where e and phi genuinely meet:
the logarithmic spiral.

    r(theta) = a * e^(b*theta)

is *golden* when its radius multiplies by phi every quarter turn:

    e^(b*pi/2) = phi   =>   b = 2*ln(phi)/pi ~= 0.3063489

So the golden spiral is literally e raised to a golden power: phi sets the
proportion, e does the growing. Every measurement in this letterform is
derived from phi; nothing is eyeballed.

Run:  python design/logo.py
Out:  design/out/*.svg  +  design/out/index.html (contact sheet)
"""

from __future__ import annotations

import math
import os

# ---------------------------------------------------------------- constants

PHI = (1.0 + math.sqrt(5.0)) / 2.0
LN_PHI = math.log(PHI)
B_GOLDEN = 2.0 * LN_PHI / math.pi  # golden spiral growth, per radian
GOLDEN_ANGLE = math.radians(360.0 / (PHI * PHI))  # 137.507...

TAU = math.tau

# ---------------------------------------------------------------- vec2 utils


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1])


def add(a, b):
    return (a[0] + b[0], a[1] + b[1])


def mul(a, s):
    return (a[0] * s, a[1] * s)


def unit(a):
    m = math.hypot(a[0], a[1]) or 1.0
    return (a[0] / m, a[1] / m)


def perp(a):
    """90 deg CCW."""
    return (-a[1], a[0])


def lerp(a, b, t):
    return a + (b - a) * t


# ---------------------------------------------------------------- path build


def tangents(pts):
    n = len(pts)
    out = []
    for i in range(n):
        if i == 0:
            d = sub(pts[1], pts[0])
        elif i == n - 1:
            d = sub(pts[-1], pts[-2])
        else:
            d = sub(pts[i + 1], pts[i - 1])
        out.append(unit(d))
    return out


def offset_side(pts, widths, side):
    """Offset a centerline by +/- half its (variable) width."""
    ts = tangents(pts)
    return [
        add(p, mul(perp(t), side * w * 0.5)) for p, t, w in zip(pts, ts, widths)
    ]


def f(x, prec=1):
    s = f"{x:.{prec}f}".rstrip("0").rstrip(".")
    return s if s not in ("", "-0") else "0"


def catmull_rom(pts, prec=1):
    """Smooth cubic-Bezier run through pts (no leading moveto)."""
    n = len(pts)
    if n < 2:
        return ""
    d = []
    for i in range(n - 1):
        p0 = pts[i - 1] if i > 0 else pts[i]
        p1, p2 = pts[i], pts[i + 1]
        p3 = pts[i + 2] if i + 2 < n else pts[i + 1]
        c1 = add(p1, mul(sub(p2, p0), 1.0 / 6.0))
        c2 = sub(p2, mul(sub(p3, p1), 1.0 / 6.0))
        d.append(
            f"C{f(c1[0], prec)} {f(c1[1], prec)} {f(c2[0], prec)} "
            f"{f(c2[1], prec)} {f(p2[0], prec)} {f(p2[1], prec)}"
        )
    return "".join(d)


def stroke_path(pts, widths, shear=(0.0, 0.0), cap0=None, cap1=None, prec=1):
    """
    Turn a variable-width centerline into a closed filled outline.

    `shear` slants the start/end caps along the tangent, as a multiple of the
    local width, giving a pen-cut terminal instead of a square one.
    `cap0`/`cap1` override a cap outright with an explicit (inner, outer) pair
    of points -- used to cut the bowl flush against the crossbar.
    """
    left = offset_side(pts, widths, +1.0)  # inner
    right = offset_side(pts, widths, -1.0)  # outer
    ts = tangents(pts)
    s0, s1 = shear
    if s0:
        left[0] = sub(left[0], mul(ts[0], s0 * widths[0]))
        right[0] = add(right[0], mul(ts[0], s0 * widths[0]))
    if s1:
        left[-1] = add(left[-1], mul(ts[-1], s1 * widths[-1]))
        right[-1] = sub(right[-1], mul(ts[-1], s1 * widths[-1]))
    if cap0:
        left[0], right[0] = cap0
    if cap1:
        left[-1], right[-1] = cap1
    right = right[::-1]
    d = f"M{f(left[0][0], prec)} {f(left[0][1], prec)}"
    d += catmull_rom(left, prec)
    d += f"L{f(right[0][0], prec)} {f(right[0][1], prec)}"
    d += catmull_rom(right, prec)
    return d + "Z"


# ---------------------------------------------------------------- letterform


def spiral_arc(theta0, theta1, r0, ratio, n=140):
    """
    Logarithmic-spiral arc. Radius starts at r0 and is multiplied by `ratio`
    across the whole sweep, i.e. r(t) = r0 * ratio**t  ==  r0 * e^(b*dtheta).
    Passing ratio = PHI**(sweep / (pi/2)) yields the *true* golden spiral.
    """
    pts, ts = [], []
    for i in range(n):
        t = i / (n - 1)
        th = lerp(theta0, theta1, t)
        r = r0 * (ratio**t)
        pts.append((r * math.cos(th), r * math.sin(th)))
        ts.append(t)
    return pts, ts


def crossbar_path(R, x_left, y_a, y_b, prec=1):
    """
    The crossbar, with its right end cut flush along the outer circle so it
    can never overshoot the bowl. Left end runs under the bowl wall.
    """
    x_a = math.sqrt(max(R * R - y_a * y_a, 0.0))
    x_b = math.sqrt(max(R * R - y_b * y_b, 0.0))
    return (
        f"M{f(x_left, prec)} {f(y_a, prec)}"
        f"L{f(x_a, prec)} {f(y_a, prec)}"
        f"A{f(R, prec)} {f(R, prec)} 0 0 1 {f(x_b, prec)} {f(y_b, prec)}"
        f"L{f(x_left, prec)} {f(y_b, prec)}Z"
    )


def build_e(
    R=500.0,
    weight=1.0 / PHI**3,  # thinnest stroke, as a fraction of R (~0.236)
    contrast=PHI,  # thick : thin ratio
    stress=360.0 / PHI**8,  # stress axis tilt, degrees (~8.2)
    aperture=360.0 / PHI**4,  # opening wedge, degrees (~52.5)
    bar_ratio=1.0,  # crossbar weight vs. thinnest bowl weight
    taper=0.0,  # 0 = flat pen cut, 1 = fully tapered terminal
    cut=0.0,  # slant of the terminal cut, in local widths
    samples=64,  # centerline samples; 64 is visually exact and keeps SVGs small
):
    """
    Lowercase 'e' on a phi grid. Nothing here is eyeballed:

      x-height   2R
      weight     R/phi^3, modulated to R/phi^3 * phi at the stress axis
      counter    the golden section of the counter places the crossbar,
                 so eye : lower counter = 1 : phi exactly
      aperture   360/phi^4 deg of wedge removed at the lower right
      stress     tilted 360/phi^8 deg off vertical

    Returns a list of SVG path 'd' strings.
    """
    w_thin = R * weight
    alpha = math.radians(stress)

    def width_at(th):
        """Vertical-stress modulation: thick on the flanks, thin top and bottom."""
        k = 0.5 + 0.5 * math.cos(2.0 * (th - alpha))
        return w_thin * (contrast**k)

    # ---- crossbar from the golden section of the counter -------------------
    # counter is widest at the thin points (top and bottom), so its half-height
    # is R - w_thin. Split the free counter height H at the golden section:
    # eye = H/phi^2 above the bar, lower counter = H/phi below it.
    r_i = R - w_thin
    w_bar = w_thin * bar_ratio
    H = 2.0 * r_i - w_bar
    y_b = r_i - H / (PHI * PHI)  # bar top edge
    y_a = y_b - w_bar  # bar bottom edge

    # ---- bowl --------------------------------------------------------------
    # The bowl starts on the outer circle exactly at the crossbar's underside
    # and sweeps all the way round; what it leaves out is exactly the aperture.
    th_start = math.asin(max(-1.0, min(1.0, y_a / R)))
    sweep = TAU - math.radians(aperture)
    th_end = th_start + sweep

    n = samples
    pts, widths = [], []
    for i in range(n):
        t = i / (n - 1)
        th = lerp(th_start, th_end, t)
        w = width_at(th)

        if taper > 0.0:
            # ease the free terminal down over the last quarter turn
            u = max(0.0, (t - (1.0 - 0.22)) / 0.22)
            s = u * u * (3.0 - 2.0 * u)  # smoothstep
            w *= lerp(1.0, 1.0 - taper * (1.0 - 1.0 / PHI), s)

        r_center = R - w * 0.5
        pts.append((r_center * math.cos(th), r_center * math.sin(th)))
        widths.append(w)

    # Cut the bowl's opening edge horizontally rather than radially, so it sits
    # flush under the crossbar and the aperture reads as one clean wedge.
    r_in0 = R - widths[0]
    cap0 = (
        (math.sqrt(max(r_in0 * r_in0 - y_a * y_a, 0.0)), y_a),
        (math.sqrt(max(R * R - y_a * y_a, 0.0)), y_a),
    )
    paths = [stroke_path(pts, widths, shear=(0.0, cut), cap0=cap0)]

    # ---- crossbar ----------------------------------------------------------
    paths.append(crossbar_path(R, -(R - w_thin * 0.5), y_a, y_b))

    return paths


def golden_spiral_path(R, turns=3.0, start_angle=math.radians(-38.0)):
    """A true golden spiral (x phi per quarter turn), for use as a motif."""
    sweep = turns * TAU
    quarter_turns = sweep / (math.pi / 2.0)
    pts, _ = spiral_arc(
        start_angle, start_angle - sweep, R, PHI**-quarter_turns, n=int(60 * turns)
    )
    d = f"M{f(pts[0][0])} {f(pts[0][1])}" + catmull_rom(pts)
    return d


# ---------------------------------------------------------------- svg output

BG = "#0a0b0e"
ACCENT = "#a78bfa"
ACCENT_2 = "#6ee7b7"


def svg_doc(body, size=1024, view=1024, extra_defs="", bg=None, radius=None):
    half = view / 2.0
    rect = ""
    if bg:
        r = f' rx="{f(radius)}"' if radius else ""
        rect = f'<rect x="0" y="0" width="{view}" height="{view}"{r} fill="{bg}"/>'
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
        f'viewBox="0 0 {view} {view}" fill="none">'
        f"{extra_defs}{rect}"
        f'<g transform="translate({f(half)} {f(half)}) scale(1 -1)">{body}</g>'
        f"</svg>"
    )


def paths_to_body(paths, fill="currentColor"):
    return "".join(f'<path d="{d}" fill="{fill}"/>' for d in paths)


# ---------------------------------------------------------------- variants


def variant(name, **kw):
    return (name, kw)


VARIANTS = [
    variant(
        "a-golden",
        title="A \u00b7 golden",
        desc="The reference cut. Weight R/\u03c6\u00b3, thick:thin = \u03c6.",
        R=380,
    ),
    variant(
        "b-contrast",
        title="B \u00b7 high contrast",
        desc="thick:thin = \u03c6\u00b2. More calligraphic, more stress.",
        R=380,
        contrast=PHI * PHI,
        weight=1.0 / PHI**3.5,
    ),
    variant(
        "c-mono",
        title="C \u00b7 monoline",
        desc="No modulation. The quietest cut, best at 16px.",
        R=380,
        contrast=1.0,
        weight=1.0 / PHI**3,
    ),
    variant(
        "d-cut",
        title="D \u00b7 pen cut",
        desc="Terminal sliced on the slant instead of square. Warmer, more written.",
        R=380,
        cut=1.0 / PHI**2,
        taper=0.5,
    ),
    variant(
        "e-open",
        title="E \u00b7 wide aperture",
        desc="Aperture opened to 360/\u03c6\u00b3 \u2248 85\u00b0. Friendlier, humanist.",
        R=380,
        aperture=360.0 / PHI**3,
        taper=1.0,
    ),
    variant(
        "f-bold",
        title="F \u00b7 bold",
        desc="Weight R/\u03c6\u00b2. Holds up as a dock tile or favicon.",
        R=380,
        weight=1.0 / PHI**2,
        contrast=PHI,
    ),
]


# ---------------------------------------------------------------- brand kit

# The chosen cut: variant A. Everything downstream derives from this.
MARK = dict(weight=1.0 / PHI**3, contrast=PHI)

INK = "#0a0b0e"
VIOLET_HI = "#c4b5fd"
VIOLET_LO = "#7c3aed"


def mark_svg(fill="currentColor", size=None, view=1000):
    """The bare mark. Its bounding box is exactly the circle, so it tiles well."""
    paths = build_e(R=view / 2.0, **MARK)
    dim = f'width="{size}" height="{size}" ' if size else ""
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" {dim}viewBox="0 0 {view} {view}">'
        f'<g transform="translate({f(view / 2)} {f(view / 2)}) scale(1 -1)">'
        f"{paths_to_body(paths, fill)}</g></svg>"
    )


def tile_svg(size=1024, spiral=True):
    """
    App tile. Squircle radius size/phi^3; the mark inset on the golden section;
    a true golden spiral etched underneath, its eye on the tile's golden point.
    """
    R_tile = size / PHI**3
    inset = size / PHI**3  # ~0.236 * size of padding all round
    m = size - 2 * inset
    motif = ""
    if spiral:
        # Anchor the spiral's eye on the tile's top-right corner so the squircle
        # clips the tight curl away and only the wide sweeps cross the face.
        arc = golden_spiral_path(size * 1.5, turns=2.5, start_angle=math.radians(196))
        motif = (
            f'<g transform="translate({f(size)} 0) scale(1 -1)" opacity="0.16">'
            f'<path d="{arc}" fill="none" stroke="#ffffff" '
            f'stroke-width="{f(size / 128)}" stroke-linecap="round"/></g>'
        )
    paths = build_e(R=m / 2.0, **MARK)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
        f'viewBox="0 0 {size} {size}">'
        f'<defs><linearGradient id="eg" x1="0" y1="0" x2="1" y2="1">'
        f'<stop offset="0" stop-color="{VIOLET_HI}"/>'
        f'<stop offset="1" stop-color="{VIOLET_LO}"/></linearGradient>'
        f'<clipPath id="ec"><rect width="{size}" height="{size}" '
        f'rx="{f(R_tile)}"/></clipPath></defs>'
        f'<g clip-path="url(#ec)">'
        f'<rect width="{size}" height="{size}" fill="url(#eg)"/>'
        f"{motif}</g>"
        f'<g transform="translate({f(size / 2)} {f(size / 2)}) scale(1 -1)">'
        f'{paths_to_body(paths, INK)}</g></svg>'
    )


def construction_svg(w=1000, h=1240):
    """
    The construction drawing: every phi-derived measurement, made visible.
    Legend sits below the figure so nothing can collide or overflow.
    """
    c = (w / 2.0, w / 2.0)
    R = w / 2.0 * 0.78

    w_thin = R * MARK["weight"]
    r_i = R - w_thin
    H = 2.0 * r_i - w_thin
    y_b = r_i - H / (PHI * PHI)
    y_a = y_b - w_thin
    th_a = math.degrees(math.asin(y_a / R))
    th_t = th_a - 360.0 / PHI**4

    mint = "#6ee7b7"
    hair = f'fill="none" stroke="{mint}" stroke-width="{f(w / 480)}"'
    faint = f'fill="none" stroke="#7c8598" stroke-width="{f(w / 700)}"'
    dash = f' stroke-dasharray="{f(w / 95)} {f(w / 115)}"'

    def ray(deg, r1):
        a = math.radians(deg)
        return (
            f'<line x1="0" y1="0" x2="{f(r1 * math.cos(a))}" '
            f'y2="{f(r1 * math.sin(a))}" {hair}/>'
        )

    fig = [
        paths_to_body(build_e(R=R, **MARK), ACCENT).replace(
            "<path", '<path opacity="0.28"'
        ),
        f'<circle cx="0" cy="0" r="{f(R)}" {faint}{dash}/>',
        f'<circle cx="0" cy="0" r="{f(r_i)}" {faint}{dash}/>',
        f'<line x1="{f(-R * 1.06)}" y1="{f(y_a)}" x2="{f(R * 1.06)}" '
        f'y2="{f(y_a)}" {faint}{dash}/>',
        f'<line x1="{f(-R * 1.06)}" y1="{f(y_b)}" x2="{f(R * 1.06)}" '
        f'y2="{f(y_b)}" {faint}{dash}/>',
        ray(th_a, R * 1.06),
        ray(th_t, R * 1.06),
        f'<path d="{golden_spiral_path(R * 0.99, turns=2.0, start_angle=math.radians(th_t))}" '
        f"{hair} stroke-opacity=\"0.85\"/>",
        f'<circle cx="0" cy="0" r="{f(w / 220)}" fill="{mint}"/>',
    ]

    mono = "ui-monospace,'Cascadia Code',Consolas,monospace"
    sans = "-apple-system,BlinkMacSystemFont,'Segoe UI',Inter,sans-serif"
    rows = [
        ("x-height", "2R", "the mark's bounding circle"),
        ("weight", "R/\u03c6\u00b3", "modulated \u00d7\u03c6 on the stress axis"),
        ("crossbar", "eye : counter = 1 : \u03c6", "golden section of the counter"),
        ("aperture", "360/\u03c6\u2074 = 52.5\u00b0", "wedge removed at the lower right"),
        ("spiral", "r = a\u00b7e^(b\u03b8), b = 2\u00b7ln\u03c6/\u03c0", "\u00d7\u03c6 every quarter turn"),
    ]
    y0 = w + 24
    legend = [
        f'<text x="{f(w * 0.08)}" y="{f(y0)}" fill="#9aa1af" font-size="{f(w / 45)}" '
        f'font-family="{sans}" letter-spacing="1.6">CONSTRUCTION</text>'
    ]
    for i, (k, val, note) in enumerate(rows):
        y = y0 + 44 + i * 30
        legend.append(
            f'<text x="{f(w * 0.08)}" y="{f(y)}" fill="#6b7280" '
            f'font-size="{f(w / 52)}" font-family="{sans}">{k}</text>'
            f'<text x="{f(w * 0.24)}" y="{f(y)}" fill="{mint}" '
            f'font-size="{f(w / 52)}" font-family="{mono}">{val}</text>'
            f'<text x="{f(w * 0.60)}" y="{f(y)}" fill="#6b7280" '
            f'font-size="{f(w / 55)}" font-family="{sans}">{note}</text>'
        )

    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
        f'viewBox="0 0 {w} {h}">'
        f'<rect width="{w}" height="{h}" fill="{BG}"/>'
        f'<g transform="translate({f(c[0])} {f(c[1])}) scale(1 -1)">'
        f'{"".join(fig)}</g>'
        f'{"".join(legend)}</svg>'
    )


def banner_svg(w=1280, h=420):
    """README header: mark, wordmark, tagline, on the app's own background."""
    m = h * 0.56  # mark size
    gap = m * 0.34
    title_px = h / 5.2
    mono_px = h / 11.5
    # rough advance-width estimates, enough to centre the lockup reliably
    text_w = max(len("agent harness") * title_px * 0.52,
                 len("r = a\u00b7e^(b\u03b8)  \u00b7  b = 2\u00b7ln\u03c6/\u03c0") * mono_px * 0.60)
    x = (w - (m + gap + text_w)) / 2.0
    tx = x + m + gap
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
        f'viewBox="0 0 {w} {h}">'
        f'<defs><radialGradient id="bg" cx="0.5" cy="0" r="0.95">'
        f'<stop offset="0" stop-color="#191331"/>'
        f'<stop offset="1" stop-color="{BG}"/></radialGradient></defs>'
        f'<rect width="{w}" height="{h}" fill="url(#bg)"/>'
        f'<svg x="{f(x)}" y="{f(h / 2 - m / 2)}" width="{f(m)}" height="{f(m)}" '
        f'viewBox="0 0 1000 1000">'
        f'<g transform="translate(500 500) scale(1 -1)">'
        f'{paths_to_body(build_e(R=500, **MARK), ACCENT)}</g></svg>'
        f'<text x="{f(tx)}" y="{f(h / 2 - h / 40)}" fill="#e7e9ee" '
        f'font-size="{f(title_px)}" font-weight="600" letter-spacing="-1.5" '
        f'font-family="-apple-system,BlinkMacSystemFont,Segoe UI,Inter,sans-serif">'
        f"agent harness</text>"
        f'<text x="{f(tx + 2)}" y="{f(h / 2 + h / 7.5)}" fill="#8b93a3" '
        f'font-size="{f(mono_px)}" '
        f'font-family="ui-monospace,Cascadia Code,Consolas,monospace">'
        f"r = a\u00b7e^(b\u03b8)  \u00b7  b = 2\u00b7ln\u03c6/\u03c0</text>"
        f"</svg>"
    )


def build_assets():
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(here)
    brand = os.path.join(here, "brand")
    os.makedirs(brand, exist_ok=True)

    written = []

    def put(path, text):
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)
        written.append(os.path.relpath(path, root))

    put(os.path.join(brand, "e-mark.svg"), mark_svg("currentColor"))
    put(os.path.join(brand, "e-mark-violet.svg"), mark_svg(ACCENT))
    put(os.path.join(brand, "e-mark-ink.svg"), mark_svg(INK))
    put(os.path.join(brand, "e-tile.svg"), tile_svg(1024))
    put(os.path.join(brand, "e-construction.svg"), construction_svg())
    put(os.path.join(brand, "e-banner.svg"), banner_svg())

    # favicon / webview icon
    put(os.path.join(root, "public", "e.svg"), mark_svg(ACCENT, view=1000))

    for p in written:
        print(f"  {p}")
    return written


def review():
    """Render the candidate contact sheet to design/out/index.html."""
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, "out")
    os.makedirs(out, exist_ok=True)

    cards = []
    for name, kw in VARIANTS:
        kw = dict(kw)
        title = kw.pop("title", name)
        desc = kw.pop("desc", "")
        paths = build_e(**kw)
        doc = svg_doc(paths_to_body(paths, "currentColor"), size=1024, view=1024)
        with open(os.path.join(out, f"{name}.svg"), "w", encoding="utf-8") as fh:
            fh.write(doc)
        cards.append((name, title, desc, doc, tile_svg(1024)))

    css = """
*{box-sizing:border-box}
body{margin:0;background:#0a0b0e;color:#e7e9ee;padding:40px 44px 96px;
 font:15px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI',Inter,Roboto,sans-serif;
 -webkit-font-smoothing:antialiased}
h1{font-size:22px;font-weight:600;margin:0 0 6px;letter-spacing:-.01em}
p.lede{color:#9aa1af;margin:0 0 8px;max-width:76ch}
code{color:#6ee7b7;font-family:ui-monospace,'Cascadia Code',Consolas,monospace;font-size:.9em}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(320px,1fr));gap:22px;margin-top:32px}
.card{background:#0e1015;border:1px solid #1d212b;border-radius:16px;overflow:hidden}
.hero{display:flex;align-items:center;justify-content:center;height:190px;color:#a78bfa}
.hero svg{width:150px;height:150px}
.meta{padding:0 20px 18px}
.n{font-weight:600;font-size:15px}
.d{color:#9aa1af;font-size:13px;margin-top:2px;min-height:40px}
.strip{display:flex;align-items:center;gap:18px;padding:16px 20px;border-top:1px solid #1d212b}
.strip.lite{background:#f4f4f6;color:#0a0b0e}
.strip svg{display:block;flex:none}
.tiles{display:flex;gap:14px;align-items:center;padding:16px 20px;border-top:1px solid #1d212b}
.tiles svg{display:block;border-radius:14px;flex:none}
.tiles svg.s2{border-radius:9px}.tiles svg.s3{border-radius:6px}
.lbl{color:#6b7280;font-size:11px;letter-spacing:.08em;text-transform:uppercase;
 margin-left:auto;font-family:ui-monospace,Consolas,monospace}
.strip.lite .lbl{color:#9aa1af}
"""

    def sized(doc, s, cls=""):
        c = f' class="{cls}"' if cls else ""
        return doc.replace(
            '<svg xmlns', f'<svg{c} xmlns', 1
        ).replace('width="1024" height="1024"', f'width="{s}" height="{s}"')

    html = [
        "<!doctype html><meta charset='utf-8'><title>e \u00b7 candidate marks</title>",
        f"<style>{css}</style>",
        "<h1>e \u2014 candidate marks</h1>",
        "<p class=lede>One construction, six \u03c6 exponents. x-height <code>2R</code>, "
        "weight <code>R/\u03c6\u00b3</code> modulated to <code>\u00d7\u03c6</code> on the "
        "stress axis, crossbar on the golden section of the counter "
        "(eye : counter = 1 : \u03c6), aperture <code>360/\u03c6\u2074</code>.</p>",
        "<p class=lede>Each card: the mark, then 32/24/16&nbsp;px on dark and on light, "
        "then the app tile at 64/40/28&nbsp;px.</p>",
        "<div class=grid>",
    ]
    for name, title, desc, doc, tile in cards:
        small = "".join(sized(doc, s) for s in (32, 24, 16))
        tiles = "".join(
            sized(tile, s, c) for s, c in ((64, ""), (40, "s2"), (28, "s3"))
        )
        html.append(
            f"<div class=card>"
            f"<div class=hero>{sized(doc, 150)}</div>"
            f"<div class=meta><div class=n>{title}</div><div class=d>{desc}</div></div>"
            f"<div class=strip style='color:#a78bfa'>{small}<span class=lbl>dark</span></div>"
            f"<div class='strip lite' style='color:#5b21b6'>{small}"
            f"<span class=lbl>light</span></div>"
            f"<div class=tiles>{tiles}<span class=lbl>tile</span></div>"
            f"</div>"
        )
    html.append("</div>")

    with open(os.path.join(out, "index.html"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(html))

    print(f"phi          = {PHI:.10f}")
    print(f"b_golden     = {B_GOLDEN:.10f}  (= 2*ln(phi)/pi)")
    print(f"golden angle = {math.degrees(GOLDEN_ANGLE):.4f} deg")
    print(f"wrote {len(cards)} marks -> {out}")


if __name__ == "__main__":
    import sys

    print(f"phi          = {PHI:.10f}")
    print(f"b_golden     = {B_GOLDEN:.10f}  (= 2*ln(phi)/pi)")
    print(f"golden angle = {math.degrees(GOLDEN_ANGLE):.4f} deg")
    if "--review" in sys.argv:
        review()
    else:
        build_assets()
