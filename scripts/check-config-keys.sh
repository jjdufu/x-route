#!/bin/sh
# XRouter: 检查 config.conf 的键、common.sh 的 KEYS、valid_kv 的 case 分支三者一致
set -e
cd "$(dirname "$0")/.."
a="$(sed -n 's/^\([A-Z_]*\)=.*/\1/p' module/config/config.conf | sort)"
b="$(sed -n 's/^KEYS="\(.*\)"/\1/p' module/scripts/common.sh | tr ' ' '\n' | sort)"
c="$(sed -n '/^valid_kv()/,/^}/p' module/scripts/common.sh | grep -oE '^ +[A-Z_|]+\)' | tr -d ' )' | tr '|' '\n' | sort -u)"
if [ "$a" = "$b" ] && [ "$a" = "$c" ]; then echo "config keys OK ($(echo "$a" | wc -l) keys)"; exit 0; fi
echo "config keys mismatch"
printf "%s\n" "--- config.conf" "$a" "--- KEYS" "$b" "--- valid_kv" "$c"
exit 1
