#!/bin/sh
# x-route: 静态检查（CI 与本地共用）
#   1. shellcheck（POSIX sh 语义，Android mksh / OpenWrt ash 均兼容）
#   2. sh -n 语法检查
#   3. 配置键一致性（scripts/check-config-keys.sh，存在时执行）
set -eu
cd "$(dirname "$0")/.."

list_sh() {
  for d in module scripts openwrt/files tests; do
    [ -d "$d" ] || continue
    find "$d" -type f \( -name '*.sh' -o -path '*/init.d/*' -o -path '*/uci-defaults/*' -o -name xr \)
  done | sort
}

files="$(list_sh)"
[ -n "${files}" ] || { echo "no shell files"; exit 0; }

echo "== shellcheck"
# SC3043: local 在 ash/mksh 中可用；SC1091: 运行时路径无法静态跟踪
sh_files=""; bash_files=""
for f in ${files}; do
  if head -n1 "$f" | grep -q bash; then bash_files="${bash_files} $f"; else sh_files="${sh_files} $f"; fi
done
# shellcheck disable=SC2086
[ -z "${sh_files}" ] || shellcheck -s sh -e SC3043,SC1091,SC1090 ${sh_files}
# shellcheck disable=SC2086
[ -z "${bash_files}" ] || shellcheck -s bash -e SC1091 ${bash_files}

echo "== sh -n"
for f in ${files}; do
  if head -n1 "$f" | grep -q bash; then bash -n "$f"; else sh -n "$f"; fi
done

for t in tests/test-*.sh; do
  [ -f "$t" ] || continue
  for s in sh mksh "busybox sh"; do
    command -v "${s%% *}" >/dev/null 2>&1 || continue
    echo "== ${t} (${s})"; ${s} "$t"
  done
done

if [ -f scripts/check-config-keys.sh ]; then
  echo "== config keys"
  sh scripts/check-config-keys.sh
fi
echo "lint OK"
