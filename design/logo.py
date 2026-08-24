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



def _build_parts(
    R=500.0,
    weight=1.0 / PHI**3,
    contrast=PHI,
    stress=360.0 / PHI**8,
    aperture=360.0 / PHI**4,
    bar_ratio=1.0,
    taper=0.0,
    cut=0.0,
    growth=PHI,  # r_end / r_start across the bowl's sweep
    samples=64,
    fit=True,
):
    """
    Lowercase 'e' whose bowl really is a logarithmic spiral.

    The outer contour is r(theta) = r0 * e^(b*theta) with b = ln(growth)/sweep,
    so `growth` is literally the factor the radius multiplies by across the
    letter. growth=1 collapses it to a circle; growth=PHI means the bowl grows
    by exactly phi from the crossbar junction round to the terminal.

    Returns the raw geometry so callers (and the construction drawing) can use
    the *same* spiral that generates the contour, rather than a decorative one.
    """
    w_thin = R * weight
    alpha = math.radians(stress)
    sweep = TAU - math.radians(aperture)

    def width_at(th):
        k = 0.5 + 0.5 * math.cos(2.0 * (th - alpha))
        return w_thin * (contrast**k)

    # Normalise so the spiral's geometric-mean radius is R; then the mark keeps
    # its size as `growth` changes, and r0/r1 straddle R.
    r0 = R * growth**-0.5
    r1 = R * growth**0.5

    # ---- crossbar, from the golden section of the counter ------------------
    r_i = R - w_thin
    w_bar = w_thin * bar_ratio
    H = 2.0 * r_i - w_bar
    y_b = r_i - H / (PHI * PHI)
    y_a = y_b - w_bar

    # The bowl opens on the spiral's first point, so r0*sin(th_start) = y_a.
    th_start = math.asin(max(-1.0, min(1.0, y_a / r0)))

    def r_at(u):
        return r0 * (growth**u)

    def th_at(u):
        return th_start + u * sweep

    def outer_at(u):
        r, th = r_at(u), th_at(u)
        return (r * math.cos(th), r * math.sin(th))

    def outer_x_at_y(y, hi=0.3):
        """Walk the spiral's opening flank to find where it crosses y."""
        lo = 0.0
        for _ in range(60):
            mid = (lo + hi) / 2.0
            if r_at(mid) * math.sin(th_at(mid)) < y:
                lo = mid
            else:
                hi = mid
        u = (lo + hi) / 2.0
        return outer_at(u), u

    # ---- bowl centreline ---------------------------------------------------
    pts, widths = [], []
    for i in range(samples):
        u = i / (samples - 1)
        w = width_at(th_at(u))
        if taper > 0.0:
            v = max(0.0, (u - 0.78) / 0.22)
            s = v * v * (3.0 - 2.0 * v)
            w *= lerp(1.0, 1.0 - taper * (1.0 - 1.0 / PHI), s)
        r = r_at(u) - w * 0.5
        pts.append((r * math.cos(th_at(u)), r * math.sin(th_at(u))))
        widths.append(w)

    # Cut the opening edge horizontally so it sits flush under the crossbar.
    r_in0 = r0 - widths[0]
    cap0 = (
        (math.sqrt(max(r_in0 * r_in0 - y_a * y_a, 0.0)), y_a),
        (r0 * math.cos(th_start), y_a),
    )

    bowl_left = offset_side(pts, widths, +1.0)
    bowl_right = offset_side(pts, widths, -1.0)
    ts = tangents(pts)
    if cut:
        bowl_left[-1] = add(bowl_left[-1], mul(ts[-1], cut * widths[-1]))
        bowl_right[-1] = sub(bowl_right[-1], mul(ts[-1], cut * widths[-1]))
    bowl_left[0], bowl_right[0] = cap0

    # ---- crossbar, right end cut flush against the spiral ------------------
    p_a, _ = outer_x_at_y(y_a)
    p_b, u_b = outer_x_at_y(y_b)
    flank = [outer_at(u_b * k / 8.0) for k in range(1, 8)]
    x_left = -(R - w_thin * 0.5)
    bar = [(x_left, y_a), (p_a[0], y_a)] + flank + [(p_b[0], y_b), (x_left, y_b)]

    # ---- the generating spiral, for the construction drawing ---------------
    spiral = [outer_at(i / 180.0) for i in range(181)]

    outlines = [bowl_left + bowl_right, bar]

    # ---- fit the whole mark back into its 2R box ---------------------------
    sx = sy = 1.0
    dx = dy = 0.0
    if fit:
        xs = [p[0] for o in outlines for p in o]
        ys = [p[1] for o in outlines for p in o]
        cx, cy = (min(xs) + max(xs)) / 2.0, (min(ys) + max(ys)) / 2.0
        span = max(max(xs) - min(xs), max(ys) - min(ys))
        sx = sy = (2.0 * R) / span if span else 1.0
        dx, dy = -cx * sx, -cy * sy

    def xf(p):
        return (p[0] * sx + dx, p[1] * sy + dy)

    return {
        "bowl": ([xf(p) for p in bowl_left], [xf(p) for p in bowl_right]),
        "bar": [xf(p) for p in bar],
        "widths": [w * sx for w in widths],
        "spiral": [xf(p) for p in spiral],
        "r0": r0 * sx,
        "r1": r1 * sx,
        "R": R,
        "growth": growth,
        "sweep": sweep,
        "th_start": th_start,
        "y_a": y_a * sy + dy,
        "y_b": y_b * sy + dy,
        "w_thin": w_thin * sx,
        "centre": (dx, dy),
        "scale": sx,
    }


def polygon_path(pts, prec=1):
    """Straight-edged closed polygon (the crossbar has hard corners)."""
    d = f"M{f(pts[0][0], prec)} {f(pts[0][1], prec)}"
    d += "".join(f"L{f(p[0], prec)} {f(p[1], prec)}" for p in pts[1:])
    return d + "Z"


def polyline_path(pts, prec=1):
    return f"M{f(pts[0][0], prec)} {f(pts[0][1], prec)}" + catmull_rom(pts, prec)


def bowl_path(left, right, prec=1):
    """
    Inner edge forward, straight terminal cut, outer edge back, straight
    opening cut. Keeping the two runs separate keeps both cuts sharp.
    """
    rev = right[::-1]
    d = f"M{f(left[0][0], prec)} {f(left[0][1], prec)}"
    d += catmull_rom(left, prec)
    d += f"L{f(rev[0][0], prec)} {f(rev[0][1], prec)}"
    d += catmull_rom(rev, prec)
    return d + "Z"


def build_e(**kw):
    """Returns a list of SVG path 'd' strings for the mark."""
    p = _build_parts(**kw)
    return [bowl_path(*p["bowl"]), polygon_path(p["bar"])]


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

# The chosen cut: variant A, on a logarithmic-spiral bowl that grows by exactly
# phi from the crossbar junction round to the terminal. Everything derives from
# this. See _build_parts for what `growth` means.
MARK = dict(weight=1.0 / PHI**3, contrast=PHI, growth=PHI)

INK = "#0a0b0e"
VIOLET_HI = "#c4b5fd"
VIOLET_LO = "#7c3aed"


def mark_svg(fill="currentColor", size=None, view=1000):
    """The bare mark, fitted so its bounding box is exactly the viewBox."""
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


def construction_svg(w=1000, h=1420):
    """
    The construction drawing. The mint curve here is not an overlay: it is the
    exact logarithmic spiral that generates the bowl's outer contour, taken
    straight from _build_parts, so it must coincide with the letter's edge.
    """
    c = (w / 2.0, w / 2.0)
    # Size the figure from the *outer* spiral radius so the r1 guide circle
    # always fits, whatever `growth` is set to.
    probe = _build_parts(R=100.0, **MARK)
    R = 100.0 * (0.40 * w) / probe["r1"]
    p = _build_parts(R=R, **MARK)
    cx, cy = p["centre"]
    r0, r1 = p["r0"], p["r1"]
    y_a, y_b = p["y_a"], p["y_b"]
    b = math.log(p["growth"]) / p["sweep"]

    mint = "#6ee7b7"
    hair = f'fill="none" stroke="{mint}" stroke-width="{f(w / 320)}"'
    thin = f'fill="none" stroke="{mint}" stroke-width="{f(w / 640)}"'
    faint = f'fill="none" stroke="#7c8598" stroke-width="{f(w / 700)}"'
    dash = f' stroke-dasharray="{f(w / 95)} {f(w / 115)}"'

    def ray(pt):
        return f'<line x1="{f(cx)}" y1="{f(cy)}" x2="{f(pt[0])}" y2="{f(pt[1])}" {thin}/>'

    start, end = p["spiral"][0], p["spiral"][-1]
    fig = [
        paths_to_body(build_e(R=R, **MARK), ACCENT).replace(
            "<path", '<path opacity="0.26"'
        ),
        # r0 and r1: the radii the spiral grows between, exactly phi apart
        f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(r0)}" {faint}{dash}/>',
        f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(r1)}" {faint}{dash}/>',
        f'<line x1="{f(cx - R * 1.16)}" y1="{f(y_a)}" x2="{f(cx + R * 1.16)}" '
        f'y2="{f(y_a)}" {faint}{dash}/>',
        f'<line x1="{f(cx - R * 1.16)}" y1="{f(y_b)}" x2="{f(cx + R * 1.16)}" '
        f'y2="{f(y_b)}" {faint}{dash}/>',
        ray(start),
        ray(end),
        # THE contour spiral
        f'<path d="{polyline_path(p["spiral"])}" {hair}/>',
        f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(w / 230)}" fill="{mint}"/>',
    ]

    mono = "ui-monospace,'Cascadia Code',Consolas,monospace"
    sans = "-apple-system,BlinkMacSystemFont,'Segoe UI',Inter,sans-serif"
    rows = [
        ("bowl", "r = r\u2080\u00b7e^(b\u03b8)", "the mint curve IS the outer contour"),
        ("growth", f"b = ln\u03c6/sweep = {b:.5f}", "radius \u00d7\u03c6 from junction to terminal"),
        ("weight", "R/\u03c6\u00b3", "modulated \u00d7\u03c6 on the stress axis"),
        ("crossbar", "eye : counter = 1 : \u03c6", "golden section of the counter"),
        ("aperture", "360/\u03c6\u2074 = 52.5\u00b0", "wedge removed at the lower right"),
    ]
    y0 = w + 30
    legend = [
        f'<text x="{f(w * 0.08)}" y="{f(y0)}" fill="#9aa1af" font-size="{f(w / 45)}" '
        f'font-family="{sans}" letter-spacing="1.6">CONSTRUCTION</text>'
    ]
    for i, (k, val, note) in enumerate(rows):
        y = y0 + 44 + i * 30
        legend.append(
            f'<text x="{f(w * 0.08)}" y="{f(y)}" fill="#6b7280" '
            f'font-size="{f(w / 52)}" font-family="{sans}">{k}</text>'
            f'<text x="{f(w * 0.22)}" y="{f(y)}" fill="{mint}" '
            f'font-size="{f(w / 52)}" font-family="{mono}">{val}</text>'
            f'<text x="{f(w * 0.58)}" y="{f(y)}" fill="#6b7280" '
            f'font-size="{f(w / 55)}" font-family="{sans}">{note}</text>'
        )
    note = [
        "The canonical golden spiral grows \u00d7\u03c6 every quarter turn,",
        "b = 2\u00b7ln\u03c6/\u03c0 \u2248 0.30635. Across this letter's 307.5\u00b0 sweep",
        "that compounds to \u03c6^3.42 \u2248 5.6\u00d7, which stops reading as an 'e'.",
        "So the bowl uses the same equation tuned to grow \u00d7\u03c6 across the",
        "whole sweep instead: b = ln\u03c6/sweep.",
    ]
    ny = y0 + 44 + len(rows) * 30 + 30
    for i, line in enumerate(note):
        legend.append(
            f'<text x="{f(w * 0.08)}" y="{f(ny + i * 24)}" fill="#6b7280" '
            f'font-size="{f(w / 62)}" font-family="{sans}">{line}</text>'
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


def growth_sheet():
    """
    Render the bowl at a range of spiral growth rates, each with the exact
    curve that generated its outer contour drawn on top. This is the check
    that the spiral claim is true, and it shows why the canonical golden rate
    cannot be used for a letterform.
    """
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, "out")
    os.makedirs(out, exist_ok=True)

    sweep = TAU - math.radians(360.0 / PHI**4)
    canonical = PHI ** (sweep / (math.pi / 2.0))
    cases = [
        ("1.000 \u2014 circle", 1.0),
        ("\u03c6^\u2153 = 1.174", PHI ** (1.0 / 3.0)),
        ("\u03c6^\u00bd = 1.272", PHI**0.5),
        ("\u03c6 = 1.618 \u2014 shipped", PHI),
        ("\u03c6\u00b2 = 2.618", PHI**2),
        (f"canonical = {canonical:.2f}", canonical),
    ]

    cards = []
    for label, g in cases:
        p = _build_parts(R=380, growth=g, **{k: v for k, v in MARK.items() if k != "growth"})
        body = (
            f'<path d="{bowl_path(*p["bowl"])}" fill="{ACCENT}" opacity="0.30"/>'
            f'<path d="{polygon_path(p["bar"])}" fill="{ACCENT}" opacity="0.30"/>'
            f'<path d="{polyline_path(p["spiral"])}" fill="none" '
            f'stroke="{ACCENT_2}" stroke-width="5"/>'
        )
        b = math.log(g) / p["sweep"] if g > 1 else 0.0
        cards.append((label, b, svg_doc(body, size=260, view=1024)))

    css = (
        "body{margin:0;background:#0a0b0e;color:#e7e9ee;padding:34px 40px 80px;"
        "font:15px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI',Inter,sans-serif}"
        "h1{font-size:21px;font-weight:600;margin:0 0 6px}"
        "p{color:#9aa1af;max-width:80ch;margin:0 0 26px}"
        "code{color:#6ee7b7;font-family:ui-monospace,Consolas,monospace;font-size:.9em}"
        ".g{display:grid;grid-template-columns:repeat(3,1fr);gap:18px}"
        ".c{background:#0e1015;border:1px solid #1d212b;border-radius:14px;"
        "padding:16px;text-align:center}"
        ".l{font-family:ui-monospace,Consolas,monospace;font-size:13px;margin-top:8px}"
        ".b{color:#6b7280;font-size:11px;font-family:ui-monospace,Consolas,monospace}"
    )
    html = [
        "<!doctype html><meta charset='utf-8'><title>e \u00b7 spiral growth</title>",
        f"<style>{css}</style>",
        "<h1>The bowl and its generating spiral</h1>",
        "<p>The mint curve is not an overlay \u2014 it is the exact logarithmic "
        "spiral the outer contour was built from, so it must coincide. "
        "<code>growth</code> is the factor the radius multiplies by across the "
        "letter's sweep. The canonical golden spiral (\u00d7\u03c6 per quarter "
        "turn) needs the last value, which is why the letter uses "
        "<code>growth = \u03c6</code> instead.</p>",
        "<div class=g>",
    ]
    for label, b, doc in cards:
        html.append(
            f"<div class=c>{doc}<div class=l>growth = {label}</div>"
            f"<div class=b>b = {b:.5f}</div></div>"
        )
    html.append("</div>")

    path = os.path.join(out, "growth.html")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(html))
    print(f"wrote {path}")


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
    elif "--growth" in sys.argv:
        growth_sheet()
    else:
        build_assets()
