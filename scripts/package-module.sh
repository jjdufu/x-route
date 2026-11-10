#!/usr/bin/env bash
# ==============================================================================
# x-route v2: 打包 SukiSU / KernelSU 模块（docs/v2-plan/11-packaging-readme.md）
#   bash scripts/package-module.sh              -> full + lite + sha256sums + RELEASE_NOTES
#   bash scripts/package-module.sh --lite-only  -> 只打 lite（不需要固件，供分支 CI 检查）
# 产物：
#   dist/x-route-sukisu-full-<ver>.zip   模块 + data/Image + data/openwrt-rootfs.img.gz
#   dist/x-route-sukisu-lite-<ver>.zip   只有模块脚本与 WebUI（使用 /data/adb/x-route/images 中已有镜像）
#   dist/openwrt/sha256sums.txt          所有发布文件的 sha256
#   dist/RELEASE_NOTES.md                Release 正文（与历史发版格式一致）
# ==============================================================================
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="${REPO}/dist"; OW="${DIST}/openwrt"
LITE_ONLY=0; [ "${1:-}" = "--lite-only" ] && LITE_ONLY=1

prop() { sed -n "s/^$1=//p" "${REPO}/module/module.prop" | tr -d '\r'; }
VER="$(prop version)"; NAME="$(prop name)"; AUTHOR="$(prop author)"
[[ "${VER}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "module.prop version 不合法：${VER}"; exit 1; }
mkdir -p "${DIST}"

ST="$(mktemp -d)"; trap 'rm -rf "${ST}"' EXIT
cp -a "${REPO}/module/." "${ST}/"
find "${ST}" -name .gitkeep -delete
rm -rf "${ST}/data"
chmod 0755 "${ST}"/*.sh "${ST}/scripts/"*.sh "${ST}/scripts/xr"
[ ! -e "${ST}/META-INF" ] || { echo "不应包含 META-INF"; exit 1; }

zipdir() { ( cd "$1" && rm -f "$2" && find . -type f | LC_ALL=C sort | sed 's#^\./##' | zip -9 -q -X "$2" -@ ); }

LITE="${DIST}/x-route-sukisu-lite-${VER}.zip"
zipdir "${ST}" "${LITE}"
echo "[+] ${LITE}"
[ "${LITE_ONLY}" = 1 ] && exit 0

for f in Image openwrt-rootfs.img.gz openwrt.qcow2 kernel.config; do
  [ -s "${OW}/$f" ] || { echo "缺少 ${OW}/$f（先运行 build-kernel.sh 与 build-rootfs.sh）"; exit 1; }
done
FULL="${DIST}/x-route-sukisu-full-${VER}.zip"
mkdir -p "${ST}/data"
cp "${OW}/Image" "${OW}/openwrt-rootfs.img.gz" "${ST}/data/"
zipdir "${ST}" "${FULL}"
echo "[+] ${FULL}"

( cd "${OW}" && sha256sum Image openwrt-rootfs.img.gz openwrt.qcow2 ) > "${OW}/sha256sums.txt"
( cd "${DIST}" && sha256sum "${FULL##*/}" "${LITE##*/}" ) >> "${OW}/sha256sums.txt"

KREL="$(sed -n 's/^# Linux\/arm64 \([0-9.]*\) Kernel Configuration$/\1/p' "${OW}/kernel.config" | head -1)"
cat > "${DIST}/RELEASE_NOTES.md" <<EOF
### ${NAME} \`${VER}\` (Author: \`${AUTHOR}\`)
- **Pre-release — NOT yet tested on a real device**: v2 is a full rewrite. It has only passed unit tests (mksh / busybox sh / dash) and QEMU \`virt\` boot smoke tests; real OnePlus 13 Gunyah pVM verification is still pending (see \`docs/TESTING.md\`)
- **One-Click Flashable Module**: \`${FULL##*/}\` (includes WebUI, \`xr\` CLI & prebuilt \`Image\` + \`openwrt-rootfs.img.gz\`)
- **Lite Module**: \`${LITE##*/}\` (scripts + WebUI only, reuses images already in \`/data/adb/x-route/images\`)
- **Firmware**: ImmortalWrt 25.12.2 (\`armsr/armv8\` ImageBuilder, \`apk\`), Linux \`${KREL:-6.12.103}\` + Gunyah pVM config; official 25.12 \`kmod-*\` packages install via \`apk\`
- **Safety Defaults**:
  - No autostart after install (\`AUTOSTART="0"\`); autostart only allowed after one successful manual start with the same config
  - \`--no-balloon --swiotlb 64 --mem 512\` + \`ulimit -l unlimited\` + \`vm_booting.fuse\` anti-bootloop protection (a device reset during VM boot disables autostart until acknowledged in WebUI)
- **Network**: \`172.29.233.0/24\` (phone \`xr0\` = \`.1\`, VM \`eth0\` = \`.254\`); LuCI \`http://172.29.233.254\`, ttyd \`:7681\`; cellular interfaces are never exposed
- **OpenVPN**: \`openvpn-openssl\` + LuCI app; import the \`.ovpn\` in LuCI, enter remote subnets manually in WebUI (\`VPN_ROUTES\`); \`terminal\` / \`site\` modes
- **Login**: user \`root\` with an **empty password** (upstream default) — set one in LuCI → System → Administration
- **Fresh Install Only**: uninstall v1 and reboot before flashing; no v1 config migration
EOF
echo "[+] ${DIST}/RELEASE_NOTES.md"
