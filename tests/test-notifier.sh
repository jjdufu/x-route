#!/bin/sh
# shellcheck disable=SC2016,SC2034
# XRouter: 通知进程检查（LSPosed 式：app_process + 隐藏接口，以 android 身份发通知，不装 APK）
#   源码约定；有 javac 时在普通 JVM 上验证文案与负载解析；有 d8/android.jar 时重编并核对已入库的 jar；有 dexdump 时核对类表
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
S="$ROOT/notifier/src/com/xrouter/notify"; J="$ROOT/module/bin/xrnotify.jar"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }

chk '[ -s "$J" ]'
# jar 里只有 classes.dex：没有清单、资源，不是 APK
if command -v unzip >/dev/null 2>&1; then
  unzip -l "$J" > "$T/jar.txt" 2>&1
  chk 'grep -q " classes.dex$" "$T/jar.txt" && ! grep -qiE "AndroidManifest|resources\.arsc|res/|META-INF" "$T/jar.txt"'
fi
# 与 lspd（LSPNotificationManager）相同的系统调用：android 包、uid 1000、带 tag 的入队与撤销
N="$S/Notifier.java"
chk 'grep -q "\"createNotificationChannelsForPackage\", PKG, SYSTEM_UID" "$N"'
chk 'grep -q "\"enqueueNotificationWithTag\", PKG, PKG, TAG, ID, n, 0" "$N"'
chk 'grep -q "\"cancelNotificationWithTag\", PKG, PKG, TAG, ID, 0" "$N"'
chk 'grep -q "PKG = \"android\"" "$N" && grep -q "SYSTEM_UID = 1000" "$N"'
chk 'grep -q "android.os.ServiceManager" "$N" && grep -q "INotificationManager\$Stub" "$N"'
chk 'grep -q "android.substName" "$N"'
# 常驻、静音的默认重要性渠道；展开样式；不可变 PendingIntent；点击打开管理器 WebUI
chk 'grep -q "IMPORTANCE_DEFAULT" "$N" && grep -q "setSound(null, null)" "$N"'
chk 'grep -q "setOngoing(true)" "$N" && grep -q "FLAG_NO_CLEAR" "$N" && grep -q "BigTextStyle" "$N"'
chk 'grep -q "FLAG_IMMUTABLE" "$N" && grep -q "ui.webui.WebUIActivity" "$N" && grep -q "ksu://webui?id=" "$N"'
chk 'grep -q "com.sukisu.ultra" "$N"'
# 保活通知只在通知栏、不上锁屏（渠道与通知都 SECRET；锁屏可见性只在建渠道时生效，所以换新渠道 ID 并删除旧渠道）
chk 'grep -q "VISIBILITY_SECRET" "$N" && grep -q "setLockscreenVisibility" "$N" && grep -q "setVisibility(Notification.VISIBILITY_SECRET)" "$N"'
chk 'grep -q "CHANNEL_ID = \"xrouter_status_v2\"" "$N" && grep -q "deleteNotificationChannel" "$N"'
# 日志导出 zip（手机上没有 zip 命令时由 jar 打包）
chk 'grep -q "\"zip\"" "$S/Main.java" && grep -q "ZipOutputStream" "$S/Zipper.java" && grep -q "com.xrouter.notify.Main zip" "$ROOT/module/scripts/xr"'
# 进程与守护进程的约定：notify.req / notify.ack / cancel
M="$S/Main.java"
chk 'grep -q "systemMain" "$M" && grep -q "\"notify.req\"" "$M" && grep -q "\"notify.ack\"" "$M"'
chk 'grep -q "notify.req" "$ROOT/module/scripts/keepalive.sh" && grep -q "notify.ack" "$ROOT/module/scripts/keepalive.sh"'
chk 'grep -q "com.xrouter.notify.Main" "$ROOT/module/scripts/keepalive.sh" && grep -q "bin/xrnotify.jar" "$ROOT/module/scripts/keepalive.sh"'

if command -v javac >/dev/null 2>&1 && command -v java >/dev/null 2>&1; then
  mkdir -p "$T/src/com/xrouter/notify"
  cp "$S/Traffic.java" "$T/src/com/xrouter/notify/"
  cat > "$T/src/com/xrouter/notify/TrafficCheck.java" <<'EOJ'
package com.xrouter.notify;
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
        System.out.println(Traffic.expanded(1024, 2048, 3072, 4096));
        Traffic.Payload p = Traffic.parse("运行中 lan 1 2 3072 5000000000");
        System.out.println(p.title() + "|" + p.collapsed());
        p = Traffic.parse("  启动中   bogus 1024 x -5 0 ");
        System.out.println(p.title() + "|" + p.mode + "|" + p.wanUp + "," + p.wanDn + "," + p.lanUp + "," + p.lanDn);
        System.out.println(Traffic.parse("运行中 total 1 2") == null);
        System.out.println(Traffic.parse("") == null);
        System.out.println(Traffic.parse(null) == null);
    }
}
EOJ
  if javac -encoding UTF-8 -d "$T/cls" "$T/src/com/xrouter/notify/"*.java 2>"$T/javac.err"; then
    java -Dfile.encoding=UTF-8 -Dsun.stdout.encoding=UTF-8 -Dstdout.encoding=UTF-8 -cp "$T/cls" com.xrouter.notify.TrafficCheck > "$T/out.txt"
    printf '%s\n' "0 B" "1023 B" "1.5 KB" "1.2 GB" "5.0 TB" "↑1.2 GB ↓8.6 GB" "公网 ↑1.0 KB ↓2.0 KB" "内网 ↑3.0 KB ↓0 B" "↑1.0 KB ↓1.0 KB" \
      "公网 ↑1.0 KB ↓2.0 KB" "内网 ↑3.0 KB ↓4.0 KB" "XRouter · 运行中|内网 ↑3.0 KB ↓4.7 GB" "XRouter · 启动中|total|1024,0,0,0" \
      true true true > "$T/want.txt"
    chk 'diff -u "$T/want.txt" "$T/out.txt"'
  else
    cat "$T/javac.err"; echo "FAIL: Traffic.java 编译失败"; fail=1
  fi
  mkdir -p "$T/zsrc/com/xrouter/notify" "$T/zcls" "$T/zd/pack/sub"
  cp "$S/Zipper.java" "$T/zsrc/com/xrouter/notify/"
  printf 'package com.xrouter.notify;\npublic class ZipCheck { public static void main(String[] a) throws Exception { Zipper.zipDir(new java.io.File(a[0]), new java.io.File(a[1]), a[2]); } }\n' > "$T/zsrc/com/xrouter/notify/ZipCheck.java"
  echo one > "$T/zd/pack/a.txt"; echo two > "$T/zd/pack/sub/b.log"
  if javac -encoding UTF-8 -d "$T/zcls" "$T/zsrc/com/xrouter/notify/"*.java 2>"$T/zjavac.err" && java -cp "$T/zcls" com.xrouter.notify.ZipCheck "$T/out.zip" "$T/zd" pack; then
    chk '[ "$(python3 -c "import zipfile,sys; z=zipfile.ZipFile(sys.argv[1]); print(\" \".join(sorted(n for n in z.namelist() if not n.endswith(\"/\"))), z.read(\"pack/sub/b.log\").decode().strip(), z.testzip())" "$T/out.zip")" = "pack/a.txt pack/sub/b.log two None" ] && [ ! -e "$T/out.zip.part" ]'
  else
    cat "$T/zjavac.err"; echo "FAIL: Zipper"; fail=1
  fi
  chk 'grep -q "1024" "$ROOT/module/scripts/traffic.sh"'
else
  echo "test-notifier: 无 javac，跳过文案检查"
fi

# 能构建时：重编结果必须与入库的 jar 一致（防止改了源码忘了重打）
AJ="${ANDROID_JAR:-${ANDROID_HOME:-}/platforms/android-35/android.jar}"
if command -v javac >/dev/null 2>&1 && command -v d8 >/dev/null 2>&1 && [ -f "$AJ" ]; then
  if ANDROID_JAR="$AJ" sh "$ROOT/notifier/build.sh" "$T/xrnotify.jar" >"$T/build.log" 2>&1; then
    chk 'cmp -s "$T/xrnotify.jar" "$J"'
  else
    cat "$T/build.log"; echo "FAIL: notifier/build.sh"; fail=1
  fi
else
  echo "test-notifier: 无 d8 / android.jar，跳过重编核对"
fi
if command -v dexdump >/dev/null 2>&1; then
  dexdump "$J" > "$T/dex.txt" 2>&1
  chk 'grep -q "Class descriptor  : '"'"'Lcom/xrouter/notify/Main;'"'"'" "$T/dex.txt"'
  chk 'grep -q "Lcom/xrouter/notify/Notifier;" "$T/dex.txt" && grep -q "Lcom/xrouter/notify/Traffic;" "$T/dex.txt"'
  chk 'grep -A3 "name *: '"'"'main'"'"'" "$T/dex.txt" | grep -q "(\[Ljava/lang/String;)V"'
fi

[ "$fail" = 0 ] && echo "test-notifier OK"
exit "$fail"
