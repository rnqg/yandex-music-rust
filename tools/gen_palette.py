"""Собирает src/palette.rs — палитры фона «Моей волны» из бандла официального приложения.

Запуск: python tools/gen_palette.py <папка с распакованным app.asar>/app/_next/static/chunks
"""

import json
import pathlib
import re
import sys

chunks = pathlib.Path(sys.argv[1])
src = next(p.read_text(encoding="utf8") for p in chunks.glob("*.js") if "PRIMARY_GRADIENT_STOPS=" in p.read_text(encoding="utf8", errors="ignore"))


def obj(name):
    i = re.search(re.escape(name) + r"=[\[{]", src).end() - 1
    depth, j = 0, i
    while True:
        c = src[j]
        if c in "{[":
            depth += 1
        elif c in "}]":
            depth -= 1
            if depth == 0:
                break
        j += 1
    text = src[i : j + 1]
    text = re.sub(r"([{,])([a-zA-Z_]+):", r'\1"\2":', text)
    return json.loads(text)


stops = obj("t.PRIMARY_GRADIENT_STOPS")
buckets = obj("t.COLOR_BUCKETS")
light = obj("t.LIGHTNESS_COLOR_BUCKETS")


def rgb(h):
    h = h.lstrip("#")
    return "[0x%s, 0x%s, 0x%s]" % (h[0:2], h[2:4], h[4:6])


out = ["//! Сгенерировано tools/gen_palette.py из бандла официального приложения. Не править руками.", ""]
out.append("/// (имя, начало оттенка, конец оттенка, основной цвет, 16 ступеней градиента)")
out.append("type Rgb = [u8; 3];")
out.append("")
out.append("pub const BUCKETS: [(&str, u16, u16, Rgb, [Rgb; 16]); %d] = [" % len(buckets))
for b in buckets:
    out.append('    ("%s", %d, %d, %s, [%s]),' % (b["name"], b["start"], b["end"], rgb(b["primary"]), ", ".join(rgb(c) for c in stops[b["name"]])))
out.append("];")
out.append("")
out.append("/// Серые цвета выбирают корзину по светлоте: (имя, от, до).")
out.append("pub const LIGHTNESS: [(&str, u8, u8); %d] = [" % len(light))
for b in light:
    out.append('    ("%s", %d, %d),' % (b["name"], b["start"], b["end"]))
out.append("];")
out.append("")
pathlib.Path(__file__).resolve().parent.parent.joinpath("src", "palette.rs").write_text("\n".join(out) + "\n", encoding="utf8")
print("ok", len(buckets))
