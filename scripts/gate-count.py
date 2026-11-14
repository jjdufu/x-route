#!/usr/bin/env python3
"""门禁计数（只计数，不是门禁）：统计门禁配置下 clippy 的真实诊断，供棘轮比较。不加任何降级标志。"""
import collections, json, subprocess, sys
from pathlib import Path

ROOT = Path(subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip())
TARGETS = [[], ["--target", "aarch64-unknown-linux-musl"]]
BASE = ROOT / "gate-baseline.json"
JUMP_MAX = 252          # 附录 A.1：lib 之外的条目数

def clippy(extra):
    cmd = ["cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--keep-going",
           "--message-format=json", *extra, "--", "-D", "warnings"]   # 与 gate.sh 第 4 步相同的级别
    out = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True).stdout
    for line in out.splitlines():
        try: m = json.loads(line)
        except ValueError: continue
        if m.get("reason") != "compiler-message": continue
        msg = m["message"]; code = (msg.get("code") or {}).get("code")
        if not code or msg["level"] not in ("warning", "error"): continue
        sp = next((s for s in msg["spans"] if s["is_primary"]), None)
        kind = "lib" if m["target"]["name"] == "xr" and "lib" in m["target"]["kind"] else "other"
        yield (code, sp["file_name"] if sp else "", sp["line_start"] if sp else 0, sp["column_start"] if sp else 0), kind

def main():
    seen, kinds = set(), collections.Counter()
    for extra in TARGETS:
        for key, kind in clippy(extra):
            if key not in seen: kinds[kind] += 1
            seen.add(key)
    guard = subprocess.run([sys.executable, "scripts/lint-guard.py"], cwd=ROOT, capture_output=True, text=True).stderr.splitlines()
    fmt = subprocess.run(["cargo", "fmt", "--all", "--", "--check"], cwd=ROOT, capture_output=True, text=True).stdout
    fmt_n = sum(1 for l in fmt.splitlines() if l.startswith("Diff in"))
    for code, n in collections.Counter(k[0] for k in seen).most_common(): print(f"{n:6d} {code}")
    total = len(seen) + len(guard) + fmt_n
    print(f"LINT {len(seen)} (xr-lib {kinds['lib']}, 其他 {kinds['other']})  GUARD {len(guard)}  FMT {fmt_n}")
    print(f"TOTAL {total}")
    if "--check" in sys.argv or "--update" in sys.argv:
        base = json.loads(BASE.read_text())
        if total > base["total"]:
            jump_ok = ("--allow-jump" in sys.argv and not base.get("jump_used") and kinds["lib"] == 0
                       and total - base["total"] <= JUMP_MAX)
            if not jump_ok: print(f"棘轮失败：{base['total']} -> {total}"); sys.exit(1)
            print(f"lib 清零后的一次性上跳：{base['total']} -> {total}（bin/集成测试首次可见）")
            base["jump_used"] = True
        if "--allow-jump" in sys.argv and kinds["lib"] == 0:
            base["jump_used"] = True        # V3 只有一次：lib 清零那次用过即作废，即使增量为 0
        if "--update" in sys.argv:
            base["total"] = total; BASE.write_text(json.dumps(base) + "\n")

main()
