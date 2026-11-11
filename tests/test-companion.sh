#!/bin/sh
# shellcheck disable=SC2016,SC2034
# XRouter: 伴生保活 APK 检查（清单与源码约定；有 javac 时编译并验证流量文案；有 aapt 时核对已打包 APK 的版本）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
C="$ROOT/companion"; S="$C/src/com/xrouter/keepalive"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }

M="$C/AndroidManifest.xml"
ver="$(sed -n 's/.*android:versionCode="\([0-9]*\)".*/\1/p' "$M" | head -1)"
chk '[ -n "$ver" ] && [ "$(cat "$ROOT/module/apk/keepalive.ver")" = "$ver" ]'
chk '[ -s "$ROOT/module/apk/keepalive.apk" ]'
# 点击跳板不导出；前台服务类型 specialUse；能看到管理器包
chk 'grep -A3 "name=\".OpenActivity\"" "$M" | grep -q "exported=\"false\""'
chk 'grep -q "Theme.NoDisplay" "$M" && grep -q "foregroundServiceType=\"specialUse\"" "$M"'
chk 'grep -q "<package android:name=\"com.sukisu.ultra\"" "$M"'
chk 'grep -q "POST_NOTIFICATIONS" "$M"'
# 通知：不可变 PendingIntent、展开样式、静音的默认重要性渠道、点击打开跳板
chk 'grep -q "FLAG_IMMUTABLE" "$S/KeepAliveService.java" && grep -q "BigTextStyle" "$S/KeepAliveService.java"'
chk 'grep -q "IMPORTANCE_DEFAULT" "$S/KeepAliveService.java" && grep -q "setSound(null, null)" "$S/KeepAliveService.java"'
chk 'grep -q "OpenActivity.class" "$S/KeepAliveService.java" && grep -q "open.req" "$S/OpenActivity.java"'
chk 'grep -q "getLongExtra(\"wan_up\"" "$S/KeepAliveService.java" && grep -q "getStringExtra(\"mode\")" "$S/KeepAliveService.java"'
# 跳板的文件名与守护进程一致
chk 'grep -q "open.req" "$ROOT/module/scripts/keepalive.sh"'

if command -v javac >/dev/null 2>&1 && command -v java >/dev/null 2>&1; then
  mkdir -p "$T/src/com/xrouter/keepalive"
  cp "$S/Traffic.java" "$T/src/com/xrouter/keepalive/"
  cat > "$T/src/com/xrouter/keepalive/TrafficCheck.java" <<'EOJ'
package com.xrouter.keepalive;
public class TrafficCheck {
    public static void main(String[] a) {
        System.out.println(Traffic.human(0));
        System.out.println(Traffic.human(1023));
        System.out.println(Traffic.human(1536));
        System.out.println(Traffic.human(1288490189L));
        System.out.println(Traffic.human(5497558138880L));
        System.out.println(Traffic.collapsed("total", 1288490189L, 9234179686L, 0, 0));
        System.out.println(Traffic.collapsed("wan", 1024, 2048, 1, 1));
        System.out.println(Traffic.collapsed("lan", 1, 1, 3072, 0));
        System.out.println(Traffic.collapsed("total", 1000, 0, 24, 1024));
        System.out.print(Traffic.expanded(1024, 2048, 3072, 4096));
    }
}
EOJ
  if javac -encoding UTF-8 -d "$T/cls" "$T/src/com/xrouter/keepalive/"*.java 2>"$T/javac.err"; then
    java -Dfile.encoding=UTF-8 -Dstdout.encoding=UTF-8 -cp "$T/cls" com.xrouter.keepalive.TrafficCheck > "$T/out.txt"
    printf '%s\n' "0 B" "1023 B" "1.5 KB" "1.2 GB" "5.0 TB" "↑1.2 GB ↓8.6 GB" "公网 ↑1.0 KB ↓2.0 KB" "内网 ↑3.0 KB ↓0 B" "↑1.0 KB ↓1.0 KB" \
      "公网 ↑1.0 KB ↓2.0 KB" "内网 ↑3.0 KB ↓4.0 KB" > "$T/want.txt"
    printf '\n' >> "$T/out.txt"
    chk 'diff -u "$T/want.txt" "$T/out.txt"'
  else
    cat "$T/javac.err"; echo "FAIL: Traffic.java 编译失败"; fail=1
  fi
  # 与 shell 侧的单位换算一致
  chk 'grep -q "1024" "$ROOT/module/scripts/traffic.sh"'
else
  echo "test-companion: 无 javac，跳过文案编译检查"
fi

if command -v aapt >/dev/null 2>&1; then
  chk 'aapt dump badging "$ROOT/module/apk/keepalive.apk" | grep -q "versionCode='"'"'$ver'"'"'"'
fi

[ "$fail" = 0 ] && echo "test-companion OK"
exit "$fail"
