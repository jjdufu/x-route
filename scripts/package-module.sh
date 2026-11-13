#!/usr/bin/env bash
# ==============================================================================
# XRouter: 打包 SukiSU / KernelSU 模块（docs/v2-plan/11-packaging-readme.md）
#   bash scripts/package-module.sh              -> full + lite + sha256sums + RELEASE_NOTES
#   bash scripts/package-module.sh --lite-only  -> 只打 lite（不需要固件，供分支 CI 检查）
# 产物：
#   dist/xrouter-sukisu-full-<ver>.zip   模块 + data/Image + data/openwrt-rootfs.img.gz
#   dist/xrouter-sukisu-lite-<ver>.zip   只有模块脚本与 WebUI（使用 /data/adb/xrouter/images 中已有镜像）
#   dist/openwrt/sha256sums.txt          所有发布文件的 sha256
#   dist/RELEASE_NOTES.md                Release 正文（与历史发版格式一致）
# ==============================================================================
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="${REPO}/dist"; OW="${DIST}/openwrt"
LITE_ONLY=0; [ "${1:-}" = "--lite-only" ] && LITE_ONLY=1

prop() { sed -n "s/^$1=//p" "${REPO}/module/module.prop" | tr -d '\r'; }
VER="$(prop version)"; NAME="$(prop name)"
[[ "${VER}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "module.prop version 不合法：${VER}"; exit 1; }
mkdir -p "${DIST}"

# WebUI：webui/（Vite + Material Web）构建到 module/webroot，产物不入库
command -v npm >/dev/null 2>&1 || { echo "需要 Node.js / npm 构建 WebUI"; exit 1; }
( cd "${REPO}/webui" && { [ -d node_modules ] || npm ci --no-fund --no-audit; } && npm run build )
for f in index.html config.json icon.png; do
  [ -s "${REPO}/module/webroot/${f}" ] || { echo "WebUI 构建产物缺少 ${f}"; exit 1; }
done

# 控制面 Rust 静态二进制（aarch64-unknown-linux-musl，产物不入库）
command -v cargo >/dev/null 2>&1 || { echo "需要 Rust / cargo 构建 bin/xr"; exit 1; }
if [ -z "${RUSTUP_HOME:-}" ] && [ -d "${HOME:-}/.rustup" ]; then export RUSTUP_HOME="${HOME}/.rustup"; fi
( cd "${REPO}/rust/xr" && cargo build --release --target aarch64-unknown-linux-musl )
XR_BIN="${REPO}/rust/xr/target/aarch64-unknown-linux-musl/release/xr"
[ -s "${XR_BIN}" ] || { echo "缺少 ${XR_BIN}"; exit 1; }

# 通知进程 jar（LSPosed 式，app_process 运行，不是 APK）：有 JDK 与 d8 时现场构建，否则使用已入库的预编译包
if command -v javac >/dev/null 2>&1 && command -v d8 >/dev/null 2>&1; then
  sh "${REPO}/notifier/build.sh" "${REPO}/module/bin/xrnotify.jar"
fi
[ -s "${REPO}/module/bin/xrnotify.jar" ] || { echo "缺少 module/bin/xrnotify.jar，请先运行 sh notifier/build.sh"; exit 1; }

ST="$(mktemp -d)"; trap 'rm -rf "${ST}"' EXIT
cp -a "${REPO}/module/." "${ST}/"
mkdir -p "${ST}/bin"
cp -f "${XR_BIN}" "${ST}/bin/xr"
chmod 0755 "${ST}/bin/xr"
if command -v file >/dev/null 2>&1; then
  file "${ST}/bin/xr" | grep -qE "ARM aarch64|ARM64" || { echo "bin/xr 非 ARM64 产物"; exit 1; }
fi
find "${ST}" -name .gitkeep -delete
rm -rf "${ST}/data"
chmod 0755 "${ST}"/*.sh "${ST}/scripts/"*.sh "${ST}/scripts/xr"
[ ! -e "${ST}/META-INF" ] || { echo "不应包含 META-INF"; exit 1; }

zipdir() { ( cd "$1" && rm -f "$2" && find . -type f | LC_ALL=C sort | sed 's#^\./##' | zip -9 -q -X "$2" -@ ); }

LITE="${DIST}/xrouter-sukisu-lite-${VER}.zip"
zipdir "${ST}" "${LITE}"
echo "[+] ${LITE}"
[ "${LITE_ONLY}" = 1 ] && exit 0

# kernel.config 只是构建记录，不打包；复用旧镜像时不一定有
for f in Image openwrt-rootfs.img.gz openwrt.qcow2; do
  [ -s "${OW}/$f" ] || { echo "缺少 ${OW}/$f（先运行 build-kernel.sh 与 build-rootfs.sh）"; exit 1; }
done
FULL="${DIST}/xrouter-sukisu-full-${VER}.zip"
mkdir -p "${ST}/data"
cp "${OW}/Image" "${OW}/openwrt-rootfs.img.gz" "${ST}/data/"
zipdir "${ST}" "${FULL}"
echo "[+] ${FULL}"

( cd "${OW}" && sha256sum Image openwrt-rootfs.img.gz openwrt.qcow2 ) > "${OW}/sha256sums.txt"
( cd "${DIST}" && sha256sum "${FULL##*/}" "${LITE##*/}" ) >> "${OW}/sha256sums.txt"

# 发版说明：干净版本号 + 自上次发版标签以来 feat/fix 提交梗概 + 必要提示
PREV_TAG="$(git -C "${REPO}" tag --list 'v*-build.*' --sort=-v:refname | head -n 1 || true)"
{
  echo "### ${NAME} ${VER}"
  echo
  if [ -n "${PREV_TAG}" ] && git -C "${REPO}" rev-parse --verify "${PREV_TAG}" >/dev/null 2>&1; then
    git -C "${REPO}" log --reverse --format='%s' "${PREV_TAG}..HEAD" \
      | awk -F': *' '/^(feat|fix)\(/ && !/^(feat|fix)\((lint|docs)\):/ { sub(/^(feat|fix)\([^)]+\): */,""); print "- " $0 }' 
  fi
  echo
  echo "仅支持全新安装。登录 root，空密码，请尽快在 LuCI 中设置。"
  echo
  echo "- 完整包：${FULL##*/}"
  echo "- 精简包：${LITE##*/}"
} > "${DIST}/RELEASE_NOTES.md"
echo "[+] ${DIST}/RELEASE_NOTES.md"
