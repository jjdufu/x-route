#!/bin/sh
# XRouter: 检查 config.conf 的键、rust/xr/src/config.rs 的 KEYS 与 valid_kv 分支三者一致
set -e
cd "$(dirname "$0")/.."
a="$(sed -n 's/^\([A-Z_]*\)=.*/\1/p' module/config/config.conf | sort)"
b="$(sed -n '/^pub const KEYS:/,/^];/p' rust/xr/src/config.rs | sed -n 's/^ *"\([A-Z0-9_]*\)".*/\1/p' | sort)"
c="$(sed -n '/^pub fn valid_kv(/,/^}/p' rust/xr/src/config.rs | grep -E '^ {8,12}(\| )?"[A-Z]' | grep -oE '"[A-Z][A-Z0-9_]+"' | tr -d '"' | sort -u)"
if [ "$a" = "$b" ] && [ "$a" = "$c" ]; then
  if [ -x module/bin/xr ]; then
    d="$(module/bin/xr internal dump-config-keys | sort)"
    if [ "$a" != "$d" ]; then
      echo "config keys mismatch (bin/xr dump-config-keys)"
      printf "%s\n" "--- config.conf" "$a" "--- bin/xr" "$d"
      exit 1
    fi
  fi
  echo "config keys OK ($(echo "$a" | wc -l) keys)"
  exit 0
fi
echo "config keys mismatch"
printf "%s\n" "--- config.conf" "$a" "--- KEYS" "$b" "--- valid_kv" "$c"
exit 1
