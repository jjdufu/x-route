#!/usr/bin/env python3
"""门禁静态守卫：扫描豁免、绕路配置和测试布局。任何发现即失败。"""
import os, re, subprocess, sys, tomllib
from pathlib import Path

ROOT = Path.cwd().resolve()
SKIP = {"target", ".git"}
errs: list[str] = []
def err(msg: str) -> None: errs.append(msg)

def walk(pred):
    for d, dirs, files in os.walk(ROOT):
        dirs[:] = [x for x in dirs if x not in SKIP]
        for f in files:
            if pred(f): yield Path(d) / f

def load(p: Path) -> dict:
    try: return tomllib.loads(p.read_text())
    except tomllib.TOMLDecodeError as e: err(f"{p}: TOML 解析失败 {e}"); return {}

def rel(p: Path) -> str: return str(p.relative_to(ROOT)) if p.is_relative_to(ROOT) else str(p)

# ---------- 1. Cargo.toml ----------
LEVELS = {"deny", "forbid"}
REQUIRED = {("rust", "warnings"), ("rust", "unsafe_code"), ("clippy", "all"), ("clippy", "pedantic"),
            ("clippy", "cargo"), ("rustdoc", "all")}
# nursery 组不得整组开启；组内除 redundant_pub_crate 外每一项都必须逐项 deny（列表取自当前工具链）
BANNED = {("clippy", "nursery"), ("clippy", "redundant_pub_crate")}
def nursery_lints() -> set[str]:
    out = subprocess.run(["clippy-driver", "-W", "help"], capture_output=True, text=True).stdout
    line = next((l for l in out.splitlines() if re.match(r"\s*clippy::nursery\s", l)), "")
    names = {n.strip().removeprefix("clippy::").replace("-", "_") for n in line.split(None, 1)[-1].split(",")}
    names.discard("")
    if not names: err("无法从 clippy-driver -W help 读取 nursery 列表")
    return names - {"redundant_pub_crate"}
root = load(ROOT / "Cargo.toml")
wl = root.get("workspace", {}).get("lints")
if not isinstance(wl, dict): err("根 Cargo.toml 缺少 [workspace.lints]")
else:
    for tool, table in wl.items():
        for name, v in table.items():
            if isinstance(v, dict):
                if set(v) - {"level", "priority"}: err(f"[workspace.lints.{tool}] {name}: 只允许 level/priority")
                level = v.get("level")
            else: level = v
            if level not in LEVELS: err(f"[workspace.lints.{tool}] {name} = {level!r}：只允许 deny/forbid")
    for tool, name in REQUIRED:
        if name not in wl.get(tool, {}): err(f"[workspace.lints.{tool}] 缺少 {name}")
    if wl.get("rust", {}).get("unsafe_code") != "forbid": err("unsafe_code 必须是 forbid")
    for tool, name in BANNED:
        if name in wl.get(tool, {}): err(f"[workspace.lints.{tool}] 不得出现 {name}（与 unreachable_pub 冲突）")
    for name in sorted(nursery_lints() - set(wl.get("clippy", {}))):
        err(f"[workspace.lints.clippy] 缺少 nursery 项 {name}（工具链升级后须补齐）")
if (ROOT / "tests").is_dir() and "package" not in root:
    err("虚拟 workspace 根目录的 tests/ 不会被编译执行，改用独立 e2e crate")
for p in walk(lambda f: f == "Cargo.toml"):
    m = load(p); t = p.read_text()
    if "cargo-features" in m: err(f"{rel(p)}: 禁止 cargo-features")
    if re.search(r"(?m)^\s*rustflags\s*=", t): err(f"{rel(p)}: 禁止 rustflags")
    if "package" in m and m.get("lints") != {"workspace": True}:
        err(f"{rel(p)}: [lints] 必须且只能是 workspace = true")
    lib = p.parent / "src" / "lib.rs"
    if "package" in m and lib.exists() and "#![doc(test(attr(deny(warnings), forbid(unsafe_code))))]" not in lib.read_text():
        err(f"{rel(lib)}: 缺少 #![doc(test(attr(deny(warnings), forbid(unsafe_code))))]")

# ---------- 2. Cargo 配置与 clippy/rustfmt 配置 ----------
BAD_BUILD = {"rustflags", "rustdocflags", "rustc", "rustc-wrapper", "rustc-workspace-wrapper", "rustdoc"}
BAD_ALIAS = {"build", "check", "clippy", "doc", "fmt", "test", "b", "c", "d", "t"}
BAD_ENV = re.compile(r"RUSTFLAGS|RUSTDOCFLAGS|RUSTC|RUSTDOC|CLIPPY|RUSTUP_TOOLCHAIN")
homes = [Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))]
cfgs = [d / ".cargo" / n for d in [ROOT, *ROOT.parents] for n in ("config", "config.toml")]
cfgs += [h / n for h in homes for n in ("config", "config.toml")]
for p in cfgs:
    if not p.is_file(): continue
    c = load(p)
    for k in BAD_BUILD & set(c.get("build", {})): err(f"{p}: 禁止 build.{k}")
    for tk, tv in c.get("target", {}).items():
        for k in {"rustflags", "rustdocflags", "runner"} & set(tv): err(f"{p}: 禁止 target.{tk}.{k}")
    for pk, pv in c.get("profile", {}).items():
        if "rustflags" in pv: err(f"{p}: 禁止 profile.{pk}.rustflags")
    for k in BAD_ALIAS & set(c.get("alias", {})): err(f"{p}: 禁止覆盖 alias.{k}")
    for k in c.get("env", {}):
        if BAD_ENV.search(k): err(f"{p}: 禁止 env.{k}")
    if "unstable" in c: err(f"{p}: 禁止 [unstable]")

# clippy.toml：必须存在于根目录且只此一份；只能收紧
STRICT = {"too-many-lines-threshold": 60, "cognitive-complexity-threshold": 15,
          "too-many-arguments-threshold": 5, "type-complexity-threshold": 150,
          "excessive-nesting-threshold": 4, "max-fn-params-bools": 1, "max-struct-bools": 2,
          "max-trait-bounds": 3, "large-error-threshold": 64, "enum-variant-size-threshold": 128,
          "single-char-binding-names-threshold": 2, "max-include-file-size": 65536}
FIXED = {"avoid-breaking-exported-api": False, "check-private-items": True, "cargo-ignore-publish": True}
EXTRA = {"disallowed-methods", "disallowed-types", "disallowed-macros", "disallowed-names"}
confs = list(walk(lambda f: f in ("clippy.toml", ".clippy.toml")))
if confs != [ROOT / "clippy.toml"]: err(f"clippy 配置必须只有根目录 clippy.toml，实际 {[rel(c) for c in confs]}")
if (ROOT / "clippy.toml").is_file():
    cc = load(ROOT / "clippy.toml")
    for k, v in cc.items():
        if k in STRICT:
            if not isinstance(v, int) or isinstance(v, bool) or not 1 <= v <= STRICT[k]:
                err(f"clippy.toml {k} = {v!r}：只能在 1..={STRICT[k]} 内收紧")
        elif k in FIXED:
            if v is not FIXED[k]: err(f"clippy.toml {k} 必须为 {FIXED[k]}")
        elif k not in EXTRA: err(f"clippy.toml 不允许的键 {k}（白名单外一律视为放宽）")
    for k in (set(STRICT) | set(FIXED)) - set(cc): err(f"clippy.toml 缺少 {k}")
for p in walk(lambda f: f in ("rustfmt.toml", ".rustfmt.toml")):
    for k in {"ignore", "disable_all_formatting", "unstable_features"} & set(load(p)):
        err(f"{rel(p)}: 禁止 {k}")

# ---------- 3. Rust 源码（去掉注释和字符串后扫描） ----------
TOKEN = re.compile(r'//[^\n]*|/\*(?:[^*]|\*(?!/))*\*/|b?r(#*)"(?:.|\n)*?"\1|b?"(?:\\.|[^"\\])*"|'
                   r"b?'(?:\\.|[^'\\\n])'", re.S)
def strip(src: str) -> str:
    return TOKEN.sub(lambda m: "\n" * m.group(0).count("\n") if m.group(0)[0] in "/" else '""', src)

RULES = [
    (r"#!?\[\s*(allow|expect|warn)\s*\(", "禁止 allow/expect/warn 属性"),
    (r"\bcfg_attr\s*\((?:[^()]|\([^()]*\))*\b(allow|expect|warn)\s*\(", "禁止 cfg_attr 包装豁免"),
    (r"\bunsafe\b", "禁止 unsafe"),
    (r"\bcfg(?:_attr)?\s*\([^\]]*?\b(clippy|rustfmt|miri|doc)\b(?!\s*[=(])", "禁止按 clippy/rustfmt/miri/doc 条件编译"),
    (r"\bcfg\s*\(\s*(any\s*\(\s*\)|false)\s*\)", "禁止用 cfg(any())/cfg(false) 隐藏代码"),
    (r"#!?\[\s*(rustfmt|clippy)\s*::", "禁止 rustfmt::/clippy:: 工具属性"),
    (r"#!?\[\s*doc\s*\(\s*hidden", "禁止 doc(hidden) 躲避 missing_docs"),
    (r"#!?\[\s*automatically_derived", "禁止 automatically_derived"),
    (r"\binclude\s*!", "禁止 include!（生成代码绕过检查）"),
    (r"#\[\s*cfg\s*\((?:[^()]|\([^()]*\))*\btest\b[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s*(?:\([^)]*\))?\s*)?mod\s+\w+\s*\{",
     "禁止内联测试模块，改用 #[cfg(test)] mod xxx; 指向独立文件"),
]
for p in walk(lambda f: f.endswith(".rs")):
    code = strip(p.read_text())
    for pat, msg in RULES:
        for m in re.finditer(pat, code):
            err(f"{rel(p)}:{code.count(chr(10), 0, m.start()) + 1}: {msg}")

for e in errs: print(f"lint-guard: {e}", file=sys.stderr)
sys.exit(1 if errs else 0)
