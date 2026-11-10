#!/usr/bin/env python3
"""XRouter WebUI: 从 Material Symbols Rounded 可变字体裁出所用图标（本地打包，不走 CDN）。

用法（需要 fonttools、brotli）：
  python3 scripts/subset-icons.py <MaterialSymbolsRounded[...].woff2> <...codepoints>
产物：src/assets/symbols.woff2（固定 wght 400 / GRAD 0 / opsz 24，保留 FILL 轴）
      src/icons.js（图标名 -> 码位，md-icon 以码位字符渲染）
图标清单：scripts/icons.txt
"""
import os, sys
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
from fontTools import subset

here = os.path.dirname(os.path.abspath(__file__))
font_in, cp_file = sys.argv[1], sys.argv[2]
names = open(os.path.join(here, "icons.txt")).read().split()
cps = dict(line.split() for line in open(cp_file) if line.strip())
use = {n: int(cps[n], 16) for n in names}

f = TTFont(font_in, lazy=False)
opts = subset.Options()
opts.flavor = "woff2"
opts.layout_features = []
opts.notdef_outline = True
opts.name_IDs = ["*"]
s = subset.Subsetter(opts)
s.populate(unicodes=sorted(set(use.values())))
s.subset(f)
f = instancer.instantiateVariableFont(f, {"wght": 400, "GRAD": 0, "opsz": 24})
out = os.path.join(here, "..", "src", "assets", "symbols.woff2")
f.flavor = "woff2"
f.save(out)
with open(os.path.join(here, "..", "src", "icons.js"), "w") as js:
    js.write("// 由 scripts/subset-icons.py 生成：图标名 -> Material Symbols 码位\n")
    js.write("export const ICONS = {\n")
    for n in names:
        js.write(f'  {n}: "\\u{{{use[n]:x}}}",\n')
    js.write("};\n")
print(out, os.path.getsize(out))
