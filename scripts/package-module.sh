#!/usr/bin/env bash
# ==============================================================================
# x-route: SuKiSU Module Zip Packager (scripts/package-module.sh)
# Packages module/ (and optional compiled OpenWrt firmware images) into a
# ready-to-flash SuKiSU / KernelSU module zip archive.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
MODULE_SRC="${REPO_ROOT}/module"
DIST_DIR="${REPO_ROOT}/dist"
OPENWRT_DIST="${DIST_DIR}/openwrt"

VERSION="$(awk -F= '/^version=/{print $2}' "${MODULE_SRC}/module.prop" | tr -d '\r ')"
VERSION="${VERSION:-v1.0.0}"

mkdir -p "${DIST_DIR}"
STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "${STAGE_DIR}"' EXIT

cp -a "${MODULE_SRC}/." "${STAGE_DIR}/"
find "${STAGE_DIR}" -name ".gitkeep" -delete
chmod +x "${STAGE_DIR}/customize.sh" "${STAGE_DIR}/service.sh" "${STAGE_DIR}/action.sh" "${STAGE_DIR}/uninstall.sh" "${STAGE_DIR}/scripts/"*.sh "${STAGE_DIR}/META-INF/com/google/android/update-binary"

# 1. Build Lite Module Zip (scripts + WebUI + keepalive, loads images from /data/adb/x-route/images)
LITE_ZIP="${DIST_DIR}/x-route-sukisu-lite-${VERSION}.zip"
rm -f "${LITE_ZIP}"
(
    cd "${STAGE_DIR}"
    zip -r9 -q "${LITE_ZIP}" .
)
echo "[+] Packaged Lite SuKiSU Module: ${LITE_ZIP} ($(du -h "${LITE_ZIP}" | awk '{print $1}'))"

# 2. If compiled OpenWrt images exist in dist/openwrt, also build Full All-in-One Module Zip!
if [[ -f "${OPENWRT_DIST}/Image" && -f "${OPENWRT_DIST}/openwrt-rootfs.img.gz" ]]; then
    mkdir -p "${STAGE_DIR}/data"
    cp -f "${OPENWRT_DIST}/Image" "${STAGE_DIR}/data/Image"
    cp -f "${OPENWRT_DIST}/openwrt-rootfs.img.gz" "${STAGE_DIR}/data/openwrt-rootfs.img.gz"
    FULL_ZIP="${DIST_DIR}/x-route-sukisu-full-${VERSION}.zip"
    rm -f "${FULL_ZIP}"
    (
        cd "${STAGE_DIR}"
        zip -r9 -q "${FULL_ZIP}" .
    )
    echo "[+] Packaged Full All-in-One SuKiSU Module (with OpenWrt firmware): ${FULL_ZIP} ($(du -h "${FULL_ZIP}" | awk '{print $1}'))"
fi
