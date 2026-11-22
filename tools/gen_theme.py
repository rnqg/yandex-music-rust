"""Собирает ui/theme.slint из CSS официального приложения.

Запуск: python tools/gen_theme.py <папка с распакованным app.asar>/app/_next/static/css
"""

import colorsys
import pathlib
import re
import sys

NAMED = {"transparent": (0, 0, 0, 0), "grey": (128, 128, 128, 1), "white": (255, 255, 255, 1), "black": (0, 0, 0, 1)}


def parse_color(s):
    s = s.strip()
    if s in NAMED:
        return NAMED[s]
    if s.startswith("#"):
        h = s[1:]
        if len(h) in (3, 4):
            h = "".join(c * 2 for c in h)
        r, g, b = int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)
        a = int(h[6:8], 16) / 255 if len(h) == 8 else 1
        return r, g, b, a
    m = re.fullmatch(r"(rgba?|hsla?)\(([^)]*)\)", s)
    if not m:
        raise ValueError(s)
    parts = [p.strip() for p in m.group(2).replace("/", ",").split(",")]
    a = float(parts[3]) if len(parts) > 3 else 1
    if m.group(1).startswith("rgb"):
        r, g, b = (round(float(p)) for p in parts[:3])
    else:
        hh = float(parts[0].rstrip("deg")) / 360
        ss = float(parts[1].rstrip("%")) / 100
        ll = float(parts[2].rstrip("%")) / 100
        r, g, b = (round(c * 255) for c in colorsys.hls_to_rgb(hh, ll, ss))
    return r, g, b, a


def slint_color(s):
    r, g, b, a = parse_color(s)
    al = round(a * 255)
    return f"#{r:02x}{g:02x}{b:02x}" + (f"{al:02x}" if al != 255 else "")


def split_top(s):
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    out.append(cur.strip())
    return out


def slint_brush(v):
    v = v.strip()
    m = re.fullmatch(r"linear-gradient\((.*)\)", v)
    if not m:
        return slint_color(v)
    args = split_top(m.group(1))
    angle = "180deg"
    if re.match(r"^-?[\d.]+(deg|turn)$", args[0]):
        a = args.pop(0)
        angle = f"{float(a[:-4]) * 360:g}deg" if a.endswith("turn") else a
    stops = []
    for i, st in enumerate(args):
        mm = re.match(r"^(.*?\))\s*([\d.]+%)?$|^(\S+)\s*([\d.]+%)?$", st)
        col = mm.group(1) or mm.group(3)
        pos = mm.group(2) or mm.group(4) or f"{i * 100 / max(1, len(args) - 1):g}%"
        stops.append(f"{slint_color(col)} {pos}")
    return f"@linear-gradient({angle}, {', '.join(stops)})"


def rem(v):
    v = v.strip()
    if v.endswith("rem"):
        return f"{float(v[:-3]) * 16:g}px"
    if v.endswith("px"):
        return v
    if v.endswith("%"):
        return None
    raise ValueError(v)


def main():
    css = "".join(p.read_text(encoding="utf8") for p in sorted(pathlib.Path(sys.argv[1]).glob("*.css")))
    rules = re.findall(r"([^{}]+)\{([^{}]*)\}", css)
    themes = {"light": {}, "dark": {}}
    root = {}
    for sel, body in rules:
        sel = sel.strip()
        decls = re.findall(r"(--ym-[a-zA-Z0-9_-]+):([^;]+)", body)
        for t in themes:
            if sel.endswith(f".ym-{t}-theme"):
                for k, v in decls:
                    themes[t].setdefault(k, v.strip())
        if sel == ":root":
            for k, v in decls:
                root.setdefault(k, v.strip())

    def resolve(v, table):
        for _ in range(5):
            v = re.sub(r"var\((--[a-zA-Z0-9_-]+)\)", lambda m: table.get(m.group(1), root.get(m.group(1), m.group(0))), v)
        return v

    out = [
        "// Сгенерировано tools/gen_theme.py из CSS официального приложения. Руками не править.",
        "",
        "export global Theme {",
        "    in-out property <bool> dark: true;",
    ]
    for k in sorted(themes["dark"]):
        if "color" not in k or k not in themes["light"]:
            continue
        try:
            light = slint_brush(resolve(themes["light"][k], themes["light"]))
            dark = slint_brush(resolve(themes["dark"][k], themes["dark"]))
        except (ValueError, AttributeError):
            continue
        name = k[5:].replace("_", "-")
        out.append(f"    out property <brush> {name}: dark ? {dark} : {light};")
    out.append("")
    sizes = sorted(k for k in root if re.match(r"--ym-(spacer-size|radius-size|font-size|font-line-height|icon-size)-", k))
    for k in sizes:
        px = rem(root[k])
        if px:
            out.append(f"    out property <length> {k[5:]}: {px};")
    dur = root.get("--ym-duration-transition", "0.3s")
    out.append(f"    out property <duration> duration-transition: {float(dur[:-1]) * 1000:g}ms;")
    out.append("}")
    pathlib.Path(__file__).resolve().parent.parent.joinpath("ui", "theme.slint").write_text("\n".join(out) + "\n", encoding="utf8")
    print("ok", len(out))


main()
