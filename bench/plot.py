"""Render the benchmark chart (SVG) from bench/results-*.jsonl.

Four small multiples, each with its own linear axis (never log bars):
CPU at ~70 processes, CPU at ~1070 processes, memory (RSS), key latency.
Writes light and dark variants in English and German:
  docs/bench-{en,de}-{light,dark}.svg

Usage: python3 bench/plot.py bench/results-2026-11-28.jsonl
"""
import json, statistics, sys, os

TOOLS = ["rbtop", "bashtop-bash", "bashtop-psutil"]
NAMES = {
    "en": {"rbtop": "rbtop", "bashtop-bash": "bashtop (bash)", "bashtop-psutil": "bashtop (psutil)"},
    "de": {"rbtop": "rbtop", "bashtop-bash": "bashtop (Bash)", "bashtop-psutil": "bashtop (psutil)"},
}
TEXT = {
    "en": {
        "title": "rbtop vs bashtop",
        "sub": "Linux VM, 2 vCPU · 160×45 terminal · 2500 ms update · mean of 2 × 60 s runs · lower is better",
        "panels": [
            ("cpu", "normal", "CPU · ~70 processes", "% of one core"),
            ("cpu", "1000", "CPU · ~1070 processes", "% of one core"),
            ("rss", "normal", "Memory (RSS) · ~70 processes", "MiB"),
            ("lat", "normal", "Key press → redraw (median)", "ms"),
        ],
    },
    "de": {
        "title": "rbtop vs. bashtop",
        "sub": "Linux-VM, 2 vCPU · Terminal 160×45 · Takt 2500 ms · Mittel aus 2 × 60 s · weniger ist besser",
        "panels": [
            ("cpu", "normal", "CPU · ~70 Prozesse", "% eines Kerns"),
            ("cpu", "1000", "CPU · ~1070 Prozesse", "% eines Kerns"),
            ("rss", "normal", "Speicher (RSS) · ~70 Prozesse", "MiB"),
            ("lat", "normal", "Tastendruck → neues Bild (Median)", "ms"),
        ],
    },
}
FIELD = {"cpu": "cpu_pct_of_core", "rss": "rss_mean_mib", "lat": "latency_ms_median"}

# Reference palette, first three categorical slots (validated all-pairs, both modes)
THEME = {
    "light": {"surface": "#ffffff", "text": "#191919", "text2": "#666666", "grid": "#e8e8e8",
              "series": ["#257de0", "#f47230", "#00b894"]},
    "dark": {"surface": "#191919", "text": "#efefef", "text2": "#9c9c9c", "grid": "#2f2f2f",
             "series": ["#338ced", "#e76624", "#00a888"]},
}

W, H = 960, 500
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"


def load(path):
    acc = {}
    for line in open(path):
        r = json.loads(line)
        tool, sc = r["name"].split("|")
        acc.setdefault((tool, sc), []).append(r)
    return {k: {f: statistics.mean(x[f] for x in v) for f in FIELD.values()} for k, v in acc.items()}


def nice_max(v):
    """Smallest 1/2/2.5/5 x 10^e that is >= v."""
    for e in range(-3, 7):
        for m in (1, 2, 2.5, 5):
            c = m * 10 ** e
            if c >= v:
                return c
    return v


def fmt(v, unit, lang="en"):
    s = _fmt(v, unit)
    return s.replace(".", ",") if lang == "de" else s


def _fmt(v, unit):
    if unit.startswith("%"):
        s = f"{v:.2f}" if v < 1 else f"{v:.1f}"
        return s + " %"
    if v < 10:
        return f"{v:.1f} {unit}"
    return f"{v:.0f} {unit}"


def bar_path(x, y, w, h, r=4):
    """Rounded data end (right), square at the baseline (left)."""
    if w <= r:
        return f'<rect x="{x:.1f}" y="{y:.1f}" width="{max(w, 1.5):.1f}" height="{h}" rx="{min(r, w / 2):.1f}"/>'
    return (f'<path d="M{x:.1f},{y:.1f} h{w - r:.1f} a{r},{r} 0 0 1 {r},{r} v{h - 2 * r} '
            f'a{r},{r} 0 0 1 -{r},{r} h-{w - r:.1f} z"/>')


def render(data, lang, mode):
    t, th, names = TEXT[lang], THEME[mode], NAMES[lang]
    o = []
    o.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" '
             f'font-family="{FONT}" role="img" aria-labelledby="t d">')
    o.append(f'<title id="t">{t["title"]}</title>')
    desc = "; ".join(
        f'{p[2]}: ' + ", ".join(f'{names[tl]} {fmt(data[(tl, p[1])][FIELD[p[0]]], p[3], lang)}' for tl in TOOLS)
        for p in t["panels"])
    o.append(f'<desc id="d">{desc}</desc>')
    o.append(f'<rect width="{W}" height="{H}" rx="8" fill="{th["surface"]}"/>')
    o.append(f'<text x="28" y="40" font-size="20" font-weight="600" fill="{th["text"]}">{t["title"]}</text>')
    o.append(f'<text x="28" y="62" font-size="12.5" fill="{th["text2"]}">{t["sub"]}</text>')
    # legend (always present for >= 2 series), plus direct labels on every bar
    lx = 28
    for i, tl in enumerate(TOOLS):
        o.append(f'<rect x="{lx}" y="78" width="12" height="12" rx="3" fill="{th["series"][i]}"/>')
        o.append(f'<text x="{lx + 18}" y="88.5" font-size="12.5" fill="{th["text"]}">{names[tl]}</text>')
        lx += 18 + int(7.1 * len(names[tl])) + 26

    pw, ph = 438, 170
    origins = [(28, 112), (494, 112), (28, 306), (494, 306)]
    label_w, bar_h, row_gap = 118, 20, 12
    # panels of the same measure share one axis (both CPU panels), so growth under load is comparable
    shared = {}
    for key, sc, _, _ in t["panels"]:
        shared[key] = max(shared.get(key, 0), max(data[(tl, sc)][FIELD[key]] for tl in TOOLS))
    for (px, py), (key, sc, title, unit) in zip(origins, t["panels"]):
        vals = [data[(tl, sc)][FIELD[key]] for tl in TOOLS]
        vmax = nice_max(shared[key] * 1.05)
        x0 = px + label_w
        aw = pw - label_w - 92  # room for value + factor at the tip
        o.append(f'<text x="{px}" y="{py + 14}" font-size="14" font-weight="600" fill="{th["text"]}">{title}</text>')
        top = py + 32
        bottom = top + 3 * bar_h + 2 * row_gap
        # recessive grid: 0, half, max
        for frac in (0, 0.5, 1):
            gx = x0 + aw * frac
            o.append(f'<line x1="{gx:.1f}" y1="{top - 6}" x2="{gx:.1f}" y2="{bottom + 6}" stroke="{th["grid"]}" stroke-width="1"/>')
            tick = vmax * frac
            ts = f"{tick:g}".replace(".", ",") if lang == "de" else f"{tick:g}"
            o.append(f'<text x="{gx:.1f}" y="{bottom + 22}" font-size="11" text-anchor="middle" fill="{th["text2"]}">{ts}</text>')
        o.append(f'<text x="{x0 + aw:.1f}" y="{bottom + 38}" font-size="11" text-anchor="end" fill="{th["text2"]}">{unit}</text>')
        rb = vals[0]
        for i, (tl, v) in enumerate(zip(TOOLS, vals)):
            y = top + i * (bar_h + row_gap)
            o.append(f'<text x="{x0 - 10}" y="{y + 14.5}" font-size="12.5" text-anchor="end" fill="{th["text"]}">{names[tl]}</text>')
            w = aw * v / vmax
            o.append(f'<g fill="{th["series"][i]}">{bar_path(x0, y, w, bar_h)}</g>')
            label = fmt(v, unit, lang)
            o.append(f'<text x="{x0 + max(w, 1.5) + 6:.1f}" y="{y + 14.5}" font-size="12.5" font-weight="600" fill="{th["text"]}">{label}</text>')
            if i > 0 and rb > 0:
                fx = x0 + max(w, 1.5) + 6 + 7.2 * len(label) + 6
                o.append(f'<text x="{fx:.1f}" y="{y + 14.5}" font-size="12" fill="{th["text2"]}">×{v / rb:.0f}</text>')
    o.append("</svg>")
    return "\n".join(o)


def main():
    data = load(sys.argv[1])
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "docs")
    os.makedirs(out, exist_ok=True)
    for lang in ("en", "de"):
        for mode in ("light", "dark"):
            p = os.path.join(out, f"bench-{lang}-{mode}.svg")
            with open(p, "w") as f:
                f.write(render(data, lang, mode))
            print(p)


if __name__ == "__main__":
    main()
