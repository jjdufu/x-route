#!/bin/sh
# 构建通知进程 module/bin/xrnotify.jar（只含 classes.dex，无资源、无清单、不是 APK）。
# 需要 JDK（javac）与 Android build-tools（d8），以及 platforms/android-35/android.jar。
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/notifier/src"
OUT="${1:-$ROOT/module/bin/xrnotify.jar}"
command -v javac >/dev/null || { echo "需要 javac"; exit 1; }
command -v d8 >/dev/null || { echo "需要 d8（Android build-tools）"; exit 1; }
ANDROID_JAR="${ANDROID_JAR:-${ANDROID_HOME:-}/platforms/android-35/android.jar}"
[ -f "$ANDROID_JAR" ] || { echo "需要 ANDROID_JAR 或 ANDROID_HOME"; exit 1; }
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/cls" "$(dirname "$OUT")"
find "$SRC" -name '*.java' | LC_ALL=C sort > "$T/sources.list"
javac -encoding UTF-8 --release 8 -cp "$ANDROID_JAR" -d "$T/cls" @"$T/sources.list" 2>&1 \
  | grep -v '^warning: \[options\]\|^1 warning$' || true
[ -f "$T/cls/com/xrouter/notify/Main.class" ] || { echo "javac 失败"; exit 1; }
( cd "$T/cls" && find . -name '*.class' | LC_ALL=C sort | xargs d8 --release --min-api 29 --lib "$ANDROID_JAR" --output "$T/out.jar" )
cp "$T/out.jar" "$OUT"
chmod 0644 "$OUT"
echo "[+] $OUT"
