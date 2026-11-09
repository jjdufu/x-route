#!/usr/bin/env bash
# ==============================================================================
# x-route: Automated ImmortalWrt 24.10 (ARM64) Minimal Firmware Builder
# Adapts kernel for OnePlus 13 (SM8750) Gunyah Protected VM (pVM) & QEMU,
# strips physical hardware bloat, injects network/firewall/NAS defaults,
# and outputs ready-to-boot Image + openwrt-rootfs.raw + openwrt.qcow2.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

IMMORTALWRT_REPO="https://github.com/immortalwrt/immortalwrt.git"
IMMORTALWRT_BRANCH="openwrt-24.10"
WORK_DIR="${REPO_ROOT}/build/immortalwrt"
OUTPUT_DIR="${REPO_ROOT}/dist/openwrt"
JOBS="$(nproc 2>/dev/null || echo 4)"
PREPARE_ONLY=0
SKIP_FEEDS=0

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --branch <branch>       ImmortalWrt branch (default: ${IMMORTALWRT_BRANCH})
  --work-dir <path>       Build working directory (default: ${WORK_DIR})
  --output-dir <path>     Output artifacts directory (default: ${OUTPUT_DIR})
  --jobs <N>              Parallel compilation jobs (default: ${JOBS})
  --prepare-only          Only clone, patch kernel/target, update feeds, and run 'make defconfig'
  --skip-feeds            Skip feeds update/install (for fast offline kernel/target config check)
  -h, --help              Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --branch) IMMORTALWRT_BRANCH="$2"; shift 2 ;;
        --work-dir) WORK_DIR="$2"; shift 2 ;;
        --output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        --jobs) JOBS="$2"; shift 2 ;;
        --prepare-only) PREPARE_ONLY=1; shift ;;
        --skip-feeds) SKIP_FEEDS=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "[ERROR] Unknown argument: $1" >&2; usage; exit 1 ;;
    esac
done

CONFIG_DIR="${REPO_ROOT}/openwrt-config"
KERNEL_FRAGMENT="${CONFIG_DIR}/kernel-gunyah-pvm.config"
DIFFCONFIG="${CONFIG_DIR}/armsr-armv8.diffconfig"
FILES_OVERLAY="${CONFIG_DIR}/files"

echo "=================================================================="
echo " x-route ImmortalWrt ARM64 Firmware Builder (Milestone M1)"
echo " Branch     : ${IMMORTALWRT_BRANCH}"
echo " Work Dir   : ${WORK_DIR}"
echo " Output Dir : ${OUTPUT_DIR}"
echo " Jobs       : ${JOBS}"
echo "=================================================================="

# 1. Clone or Update ImmortalWrt Source Tree
if [[ ! -d "${WORK_DIR}/.git" ]]; then
    mkdir -p "$(dirname "${WORK_DIR}")"
    echo "[1/6] Cloning ImmortalWrt (${IMMORTALWRT_BRANCH})..."
    git clone --depth=1 -b "${IMMORTALWRT_BRANCH}" "${IMMORTALWRT_REPO}" "${WORK_DIR}"
else
    echo "[1/6] Using existing ImmortalWrt tree at ${WORK_DIR}"
fi

# 2. Trim Physical Hardware Bloat in target/linux/armsr
echo "[2/6] Patching target/linux/armsr to strip physical NIC & U-Boot bloat..."
ARMSR_MAKEFILE="${WORK_DIR}/target/linux/armsr/Makefile"
ARMSR_IMAGE_MAKEFILE="${WORK_DIR}/target/linux/armsr/image/Makefile"

if [[ -f "${ARMSR_MAKEFILE}" ]]; then
    sed -i 's/DEFAULT_PACKAGES += mkf2fs e2fsprogs automount autocore/DEFAULT_PACKAGES += e2fsprogs autocore/' "${ARMSR_MAKEFILE}"
fi

if [[ -f "${ARMSR_IMAGE_MAKEFILE}" ]]; then
    python3 - "${ARMSR_IMAGE_MAKEFILE}" <<'PYEOF'
import re, sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    content = f.read()

# Remove UBOOT build requirement and replace physical NIC kmods in Device/generic
content = re.sub(r"^\s*UBOOT\s*:=\s*.*$", "", content, flags=re.MULTILINE)
content = re.sub(
    r"DEVICE_PACKAGES\s*\+=.*?(?=\nendef)",
    "DEVICE_PACKAGES += kmod-virtio-net kmod-virtio-blk kmod-fs-9p kmod-fs-virtiofs",
    content,
    flags=re.DOTALL,
)
with open(path, "w", encoding="utf-8") as f:
    f.write(content)
PYEOF
fi

# 3. Inject Qualcomm Gunyah Protected VM (pVM) & VirtIO Kernel Config
echo "[3/6] Injecting Gunyah pVM kernel config (CONFIG_DMA_RESTRICTED_POOL=y, virtio-fs, 9p)..."
merge_kernel_config() {
    local target_cfg="$1"
    [[ -f "${target_cfg}" ]] || return 0
    while IFS= read -r line; do
        [[ -z "${line}" || "${line}" =~ ^[[:space:]]*#.* ]] && continue
        if [[ "${line}" =~ ^(CONFIG_[A-Za-z0-9_]+)= ]]; then
            local sym="${BASH_REMATCH[1]}"
            sed -i "/^${sym}=/d; /^# ${sym} is not set/d" "${target_cfg}"
            echo "${line}" >> "${target_cfg}"
        fi
    done < "${KERNEL_FRAGMENT}"
    # Handle explicit '# CONFIG_XXX is not set' lines
    while IFS= read -r line; do
        if [[ "${line}" =~ ^#[[:space:]]+(CONFIG_[A-Za-z0-9_]+)[[:space:]]+is[[:space:]]+not[[:space:]]+set ]]; then
            local sym="${BASH_REMATCH[1]}"
            sed -i "/^${sym}=/d; /^# ${sym} is not set/d" "${target_cfg}"
            echo "${line}" >> "${target_cfg}"
        fi
    done < "${KERNEL_FRAGMENT}"
}

for kcfg in "${WORK_DIR}"/target/linux/armsr/config-* "${WORK_DIR}"/target/linux/armsr/armv8/config-*; do
    merge_kernel_config "${kcfg}"
done

# 4. Update & Install Feeds
if [[ "${SKIP_FEEDS}" -eq 0 ]]; then
    echo "[4/6] Updating and installing ImmortalWrt feeds..."
    (
        cd "${WORK_DIR}"
        ./scripts/feeds update -a
        ./scripts/feeds install -a
    )
else
    echo "[4/6] Skipping feeds update (--skip-feeds specified)."
fi

# 5. Copy Overlay Files & Generate .config
echo "[5/6] Applying x-route rootfs overlay and diffconfig..."
rm -rf "${WORK_DIR}/files"
cp -a "${FILES_OVERLAY}" "${WORK_DIR}/files"
chmod +x "${WORK_DIR}/files/etc/uci-defaults/"* "${WORK_DIR}/files/etc/init.d/"* 2>/dev/null || true

cp "${DIFFCONFIG}" "${WORK_DIR}/.config"
(
    cd "${WORK_DIR}"
    make defconfig
)

echo "[INFO] Verifying critical symbols in generated configuration..."
grep -E "CONFIG_DMA_RESTRICTED_POOL|CONFIG_SWIOTLB|CONFIG_VIRTIO_FS|CONFIG_9P_FS" "${WORK_DIR}"/target/linux/armsr/armv8/config-*
grep -E "CONFIG_TARGET_armsr_armv8_DEVICE_generic=y|CONFIG_PACKAGE_luci=y|CONFIG_PACKAGE_openvpn-openssl=y|CONFIG_PACKAGE_openlist=y" "${WORK_DIR}/.config" || true

if [[ "${PREPARE_ONLY}" -eq 1 ]]; then
    echo "[SUCCESS] Preparation and configuration verification complete (--prepare-only)."
    exit 0
fi

# 6. Compile Kernel & Rootfs Images
echo "[6/6] Downloading sources and compiling ImmortalWrt (${JOBS} threads)..."
(
    cd "${WORK_DIR}"
    make download -j"${JOBS}"
    make -j"${JOBS}" V=s
)

# Package Artifacts for SuKiSU Module (crosvm Gunyah + QEMU)
mkdir -p "${OUTPUT_DIR}"
BIN_DIR="${WORK_DIR}/bin/targets/armsr/armv8"

cp -f "${BIN_DIR}/immortalwrt-armsr-armv8-generic-kernel.bin" "${OUTPUT_DIR}/Image"
cp -f "${BIN_DIR}/immortalwrt-armsr-armv8-generic-ext4-rootfs.img.gz" "${OUTPUT_DIR}/openwrt-rootfs.img.gz"
gzip -dc "${OUTPUT_DIR}/openwrt-rootfs.img.gz" > "${OUTPUT_DIR}/openwrt-rootfs.raw"

if command -v qemu-img >/dev/null 2>&1; then
    qemu-img convert -f raw -O qcow2 -c "${OUTPUT_DIR}/openwrt-rootfs.raw" "${OUTPUT_DIR}/openwrt.qcow2"
fi

(
    cd "${OUTPUT_DIR}"
    sha256sum Image openwrt-rootfs.img.gz openwrt-rootfs.raw $( [[ -f openwrt.qcow2 ]] && echo openwrt.qcow2 ) > sha256sums.txt
)

echo "=================================================================="
echo " Build Complete! Artifacts available in: ${OUTPUT_DIR}"
ls -lh "${OUTPUT_DIR}"
echo "=================================================================="
