#!/bin/sh
# 构建伴生保活 APK 到 module/apk/keepalive.apk。需要 JDK 与 Android build-tools（aapt/d8/apksigner）。
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/companion"
OUT="${1:-$ROOT/module/apk/keepalive.apk}"
command -v javac >/dev/null || { echo "需要 javac"; exit 1; }
command -v d8 >/dev/null || command -v dx >/dev/null || { echo "需要 d8 或 dx"; exit 1; }
command -v aapt >/dev/null || command -v aapt2 >/dev/null || { echo "需要 aapt"; exit 1; }
ANDROID_JAR="${ANDROID_JAR:-${ANDROID_HOME:-}/platforms/android-35/android.jar}"
[ -f "$ANDROID_JAR" ] || ANDROID_JAR="${ANDROID_HOME:-}/platforms/android-34/android.jar"
[ -f "$ANDROID_JAR" ] || { echo "需要 ANDROID_JAR 或 ANDROID_HOME"; exit 1; }
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/src" "$T/cls" "$T/res"
cp -a "$SRC/src/." "$T/src/"
cp "$SRC/AndroidManifest.xml" "$T/AndroidManifest.xml"
# 占位资源，供 aapt 打包
mkdir -p "$T/res/values"
echo '<resources><string name="app_name">XRouter</string></resources>' > "$T/res/values/strings.xml"
AAPT="$(command -v aapt)"
"$AAPT" package -f -M "$T/AndroidManifest.xml" -S "$T/res" -I "$ANDROID_JAR" -F "$T/base.apk" --min-sdk-version 29 --target-sdk-version 35
"$AAPT" package -f -M "$T/AndroidManifest.xml" -S "$T/res" -I "$ANDROID_JAR" -m -J "$T/src"
find "$T/src" -name '*.java' > "$T/sources.list"
javac --release 8 -cp "$ANDROID_JAR" -d "$T/cls" @"$T/sources.list"
if command -v d8 >/dev/null; then
  ( cd "$T/cls" && find . -name '*.class' | xargs d8 --output "$T" --min-api 29 )
else
  dx --dex --output="$T/classes.dex" "$T/cls"
fi
[ -f "$T/classes.dex" ]
unzip -q -d "$T/apk" "$T/base.apk"
cp "$T/classes.dex" "$T/apk/"
( cd "$T/apk" && zip -qr "$T/unsigned.apk" . )
# 调试证书仅用于模块内安装（root pm install），不是应用商店发布密钥
if [ ! -f "$SRC/debug.keystore" ]; then
  keytool -genkeypair -keystore "$SRC/debug.keystore" -storepass android -keypass android \
    -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
    -dname "CN=XRouter,O=XRouter,C=CN" >/dev/null
fi
apksigner sign --ks "$SRC/debug.keystore" --ks-pass pass:android --key-pass pass:android \
  --out "$OUT" "$T/unsigned.apk"
rm -f "${OUT}.idsig"
echo "[+] $OUT"
