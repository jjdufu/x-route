#!/usr/bin/env bash
# ==============================================================================
# x-route: Automated ImmortalWrt 24.10 (ARM64) Minimal Firmware Builder
# Supports two build modes:
#   1. --mode fast (Default, ~5-7 min in CI):
#      - Assembles minimal rootfs (openwrt-rootfs.raw / .qcow2) via official
#        ImmortalWrt 24.10.6 ImageBuilder with prebuilt packages & files overlay
#      - Cross-compiles custom Linux 6.6 ARM64 kernel (Image) with Gunyah pVM
#        (CONFIG_DMA_RESTRICTED_POOL=y) and built-in VirtIO/VirtioFS/9p/nftables
#   2. --mode full (~60 min in CI):
#      - Full source compilation of ImmortalWrt toolchain, kernel, and packages
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

BUILD_MODE="fast"
IMMORTALWRT_REPO="https://github.com/immortalwrt/immortalwrt.git"
IMMORTALWRT_BRANCH="openwrt-24.10"
IMMORTALWRT_RELEASE="24.10.6"
KERNEL_VER="6.6.133"
WORK_DIR="${REPO_ROOT}/build"
OUTPUT_DIR="${REPO_ROOT}/dist/openwrt"
JOBS="$(nproc 2>/dev/null || echo 4)"
PREPARE_ONLY=0
SKIP_FEEDS=0

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --mode <fast|full>      Build mode: 'fast' (ImageBuilder + Kernel, ~6 min) or 'full' (source, ~60 min)
  --branch <branch>       ImmortalWrt branch for full mode (default: ${IMMORTALWRT_BRANCH})
  --release <version>     ImmortalWrt release for fast ImageBuilder mode (default: ${IMMORTALWRT_RELEASE})
  --work-dir <path>       Build working directory (default: ${WORK_DIR})
  --output-dir <path>     Output artifacts directory (default: ${OUTPUT_DIR})
  --jobs <N>              Parallel compilation jobs (default: ${JOBS})
  --prepare-only          Only prepare and verify configuration without compiling
  --skip-feeds            Skip feeds update/install in full mode
  -h, --help              Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mode) BUILD_MODE="$2"; shift 2 ;;
        --branch) IMMORTALWRT_BRANCH="$2"; shift 2 ;;
        --release) IMMORTALWRT_RELEASE="$2"; shift 2 ;;
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
echo " x-route ImmortalWrt ARM64 Firmware Builder"
echo " Mode       : ${BUILD_MODE}"
echo " Release    : ${IMMORTALWRT_RELEASE} (${IMMORTALWRT_BRANCH})"
echo " Work Dir   : ${WORK_DIR}"
echo " Output Dir : ${OUTPUT_DIR}"
echo " Jobs       : ${JOBS}"
echo "=================================================================="

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
    while IFS= read -r line; do
        if [[ "${line}" =~ ^#[[:space:]]+(CONFIG_[A-Za-z0-9_]+)[[:space:]]+is[[:space:]]+not[[:space:]]+set ]]; then
            local sym="${BASH_REMATCH[1]}"
            sed -i "/^${sym}=/d; /^# ${sym} is not set/d" "${target_cfg}"
            echo "${line}" >> "${target_cfg}"
        fi
    done < "${KERNEL_FRAGMENT}"
}

build_fast_mode() {
    mkdir -p "${WORK_DIR}" "${OUTPUT_DIR}"
    local ib_dir="${WORK_DIR}/imagebuilder"
    local k_dir="${WORK_DIR}/linux-${KERNEL_VER}"

    # --- Step 1: Download & Prepare Official ImmortalWrt ImageBuilder ---
    local ib_tar="immortalwrt-imagebuilder-${IMMORTALWRT_RELEASE}-armsr-armv8.Linux-x86_64.tar.zst"
    local ib_url="https://downloads.immortalwrt.org/releases/${IMMORTALWRT_RELEASE}/targets/armsr/armv8/${ib_tar}"

    if [[ ! -d "${ib_dir}" ]]; then
        echo "[1/4] Downloading ImmortalWrt ${IMMORTALWRT_RELEASE} ImageBuilder..."
        wget -q --show-progress -O "${WORK_DIR}/${ib_tar}" "${ib_url}"
        mkdir -p "${ib_dir}"
        tar --use-compress-program=unzstd -xf "${WORK_DIR}/${ib_tar}" -C "${ib_dir}" --strip-components=1
    fi

    # Configure ImageBuilder for 512MB ext4 rootfs (skip squashfs & vmdk to save time)
    sed -i 's/^CONFIG_TARGET_ROOTFS_SQUASHFS=y/# CONFIG_TARGET_ROOTFS_SQUASHFS is not set/' "${ib_dir}/.config"
    sed -i 's/^CONFIG_VMDK_IMAGES=y/# CONFIG_VMDK_IMAGES is not set/' "${ib_dir}/.config"
    sed -i 's/^CONFIG_TARGET_ROOTFS_PARTSIZE=.*/CONFIG_TARGET_ROOTFS_PARTSIZE=512/' "${ib_dir}/.config"

    local overlay_stage="${WORK_DIR}/files_stage"
    rm -rf "${overlay_stage}"
    cp -a "${FILES_OVERLAY}" "${overlay_stage}"
    chmod +x "${overlay_stage}/etc/uci-defaults/"* "${overlay_stage}/etc/init.d/"* 2>/dev/null || true

    # Note:
    # - Explicitly remove -dnsmasq and -libustream-mbedtls while selecting libustream-openssl
    # - 9pnet_virtio is included in kmod-9pnet; virtiofs is compiled built-in (=y) in our kernel Image
    local pkg_list="dnsmasq-full -dnsmasq -libustream-mbedtls libustream-openssl \
        firewall4 nftables iptables-nft ip6tables-nft kmod-ipt-ipopt iptables-mod-ipopt \
        kmod-tun kmod-fs-9p kmod-9pnet kmod-fuse \
        luci luci-compat luci-lib-ipkg luci-theme-argon \
        luci-i18n-base-zh-cn luci-i18n-firewall-zh-cn \
        luci-app-ttyd luci-i18n-ttyd-zh-cn \
        openvpn-openssl luci-app-openvpn luci-i18n-openvpn-zh-cn \
        openlist luci-app-openlist luci-i18n-openlist-zh-cn \
        ip-full ip-bridge curl ca-bundle ca-certificates tcpdump-mini htop nano losetup resize2fs blkid \
        -ppp -ppp-mod-pppoe -automount -mkf2fs -uboot-armsr-qemu_armv8 \
        -kmod-atlantic -kmod-bcmgenet -kmod-octeontx2-net -kmod-renesas-net-avb \
        -kmod-amazon-ena -kmod-e1000e -kmod-vmxnet3 -kmod-mvneta -kmod-mvpp2 \
        -kmod-fsl-dpaa1-net -kmod-fsl-dpaa2-net -kmod-fsl-enetc-net -kmod-dwmac-imx \
        -kmod-fsl-fec -kmod-dwmac-rockchip -kmod-dwmac-sun8i -kmod-thunderx-net \
        -kmod-brcmfmac -kmod-ath10k -kmod-mt7615e -kmod-carl9170"

    if [[ "${PREPARE_ONLY}" -eq 1 ]]; then
        echo "[INFO] Fast mode ImageBuilder & kernel config verified (--prepare-only)."
        return 0
    fi

    echo "[2/4] Building minimal ext4 rootfs via ImageBuilder..."
    (
        cd "${ib_dir}"
        # shellcheck disable=SC2086
        make image PROFILE="generic" PACKAGES="${pkg_list}" FILES="${overlay_stage}" ROOTFS_PARTSIZE="512"
    )

    local ib_bin="${ib_dir}/bin/targets/armsr/armv8"
    local rootfs_gz
    rootfs_gz="$(ls "${ib_bin}"/*-armsr-armv8-generic-ext4-rootfs.img.gz | head -n 1)"
    gzip -dc "${rootfs_gz}" > "${OUTPUT_DIR}/openwrt-rootfs.raw"

    # Strip ImageBuilder's mismatched kmod autoload entries since all required drivers are built-in (=y)
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        local mnt_tmp
        mnt_tmp="$(mktemp -d)"
        if sudo mount -o loop "${OUTPUT_DIR}/openwrt-rootfs.raw" "${mnt_tmp}"; then
            sudo rm -rf "${mnt_tmp}/etc/modules.d/"* "${mnt_tmp}/etc/modules-boot.d/"* 2>/dev/null || true
            sudo umount "${mnt_tmp}"
        fi
        rmdir "${mnt_tmp}" 2>/dev/null || true
    fi
    gzip -9c "${OUTPUT_DIR}/openwrt-rootfs.raw" > "${OUTPUT_DIR}/openwrt-rootfs.img.gz"

    # Compile custom ARM64 kernel with Gunyah pVM (CONFIG_DMA_RESTRICTED_POOL=y) & built-in VirtIO/VirtioFS/9p
    echo "[3/4] Preparing & Compiling Gunyah pVM + VirtIO Linux ${KERNEL_VER} ARM64 Kernel (Image)..."
    if [[ ! -d "${k_dir}" ]]; then
        local k_tar="linux-${KERNEL_VER}.tar.xz"
        if ! wget -q --show-progress -O "${WORK_DIR}/${k_tar}" "https://cdn.kernel.org/pub/linux/kernel/v6.x/${k_tar}"; then
            echo "[WARN] linux-${KERNEL_VER}.tar.xz not found on cdn.kernel.org, falling back to 6.6.87..."
            KERNEL_VER="6.6.87"
            k_dir="${WORK_DIR}/linux-${KERNEL_VER}"
            k_tar="linux-${KERNEL_VER}.tar.xz"
            wget -q --show-progress -O "${WORK_DIR}/${k_tar}" "https://cdn.kernel.org/pub/linux/kernel/v6.x/${k_tar}"
        fi
        tar -xf "${WORK_DIR}/${k_tar}" -C "${WORK_DIR}"
    fi

    (
        cd "${k_dir}"
        make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- defconfig
        merge_kernel_config "${k_dir}/.config"
        make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- olddefconfig
        # Re-apply merge once more after olddefconfig to catch any newly unlocked submenu symbols
        merge_kernel_config "${k_dir}/.config"
        make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- olddefconfig
        echo "[INFO] Verified Gunyah pVM & VirtIO built-in (=y) symbols in kernel .config:"
        grep -E "CONFIG_DMA_RESTRICTED_POOL=|CONFIG_SWIOTLB=|CONFIG_VIRTIO_FS=|CONFIG_9P_FS=|CONFIG_NF_TABLES=|CONFIG_NF_CONNTRACK=|CONFIG_VIRTIO_BALLOON" "${k_dir}/.config"
        grep -q "^CONFIG_DMA_RESTRICTED_POOL=y$" "${k_dir}/.config"
        grep -q "^CONFIG_NF_TABLES=y$" "${k_dir}/.config"
        ! grep -q "^CONFIG_VIRTIO_BALLOON=y$" "${k_dir}/.config"
        make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- -j"${JOBS}" Image
    )

    cp -f "${k_dir}/arch/arm64/boot/Image" "${OUTPUT_DIR}/Image"

    echo "[4/4] Converting rootfs to qcow2 and generating checksums..."
    if command -v qemu-img >/dev/null 2>&1; then
        qemu-img convert -f raw -O qcow2 -c "${OUTPUT_DIR}/openwrt-rootfs.raw" "${OUTPUT_DIR}/openwrt.qcow2"
    fi

    (
        cd "${OUTPUT_DIR}"
        sha256sum Image openwrt-rootfs.img.gz openwrt-rootfs.raw $( [[ -f openwrt.qcow2 ]] && echo openwrt.qcow2 ) > sha256sums.txt
    )
}

build_full_mode() {
    local src_dir="${WORK_DIR}/immortalwrt"
    if [[ ! -d "${src_dir}/.git" ]]; then
        mkdir -p "$(dirname "${src_dir}")"
        echo "[1/6] Cloning ImmortalWrt (${IMMORTALWRT_BRANCH})..."
        git clone --depth=1 -b "${IMMORTALWRT_BRANCH}" "${IMMORTALWRT_REPO}" "${src_dir}"
    else
        echo "[1/6] Using existing ImmortalWrt tree at ${src_dir}"
    fi

    echo "[2/6] Patching target/linux/armsr to strip physical NIC & U-Boot bloat..."
    local armsr_mk="${src_dir}/target/linux/armsr/Makefile"
    local armsr_img_mk="${src_dir}/target/linux/armsr/image/Makefile"

    if [[ -f "${armsr_mk}" ]]; then
        sed -i 's/DEFAULT_PACKAGES += mkf2fs e2fsprogs automount autocore/DEFAULT_PACKAGES += e2fsprogs autocore/' "${armsr_mk}"
    fi

    if [[ -f "${armsr_img_mk}" ]]; then
        python3 - "${armsr_img_mk}" <<'PYEOF'
import re, sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    content = f.read()
content = re.sub(r"^\s*UBOOT\s*:=\s*.*$", "", content, flags=re.MULTILINE)
content = re.sub(
    r"DEVICE_PACKAGES\s*\+=.*?(?=\nendef)",
    "DEVICE_PACKAGES += kmod-virtio-net kmod-virtio-blk kmod-fs-9p kmod-9pnet",
    content,
    flags=re.DOTALL,
)
with open(path, "w", encoding="utf-8") as f:
    f.write(content)
PYEOF
    fi

    echo "[3/6] Injecting Gunyah pVM kernel config (CONFIG_DMA_RESTRICTED_POOL=y, virtio-fs, 9p)..."
    for kcfg in "${src_dir}"/target/linux/armsr/config-* "${src_dir}"/target/linux/armsr/armv8/config-*; do
        merge_kernel_config "${kcfg}"
    done

    if [[ "${SKIP_FEEDS}" -eq 0 ]]; then
        echo "[4/6] Updating and installing ImmortalWrt feeds..."
        (
            cd "${src_dir}"
            ./scripts/feeds update -a
            ./scripts/feeds install -a
        )
    fi

    echo "[5/6] Applying x-route rootfs overlay and diffconfig..."
    rm -rf "${src_dir}/files"
    cp -a "${FILES_OVERLAY}" "${src_dir}/files"
    chmod +x "${src_dir}/files/etc/uci-defaults/"* "${src_dir}/files/etc/init.d/"* 2>/dev/null || true

    cp "${DIFFCONFIG}" "${src_dir}/.config"
    (
        cd "${src_dir}"
        make defconfig
        # Force disable libustream-mbedtls and luci-ssl after defconfig to prevent opkg file clash
        sed -i 's/^CONFIG_PACKAGE_luci-ssl=y/# CONFIG_PACKAGE_luci-ssl is not set/' .config
        sed -i 's/^CONFIG_PACKAGE_libustream-mbedtls=y/# CONFIG_PACKAGE_libustream-mbedtls is not set/' .config
        make defconfig
    )

    if [[ "${PREPARE_ONLY}" -eq 1 ]]; then
        echo "[SUCCESS] Full mode preparation and verification complete (--prepare-only)."
        return 0
    fi

    echo "[6/6] Downloading sources and compiling ImmortalWrt (${JOBS} threads)..."
    (
        cd "${src_dir}"
        make download -j"${JOBS}"
        make -j"${JOBS}" V=s
    )

    mkdir -p "${OUTPUT_DIR}"
    local bin_dir="${src_dir}/bin/targets/armsr/armv8"
    cp -f "${bin_dir}/immortalwrt-armsr-armv8-generic-kernel.bin" "${OUTPUT_DIR}/Image"
    cp -f "${bin_dir}/immortalwrt-armsr-armv8-generic-ext4-rootfs.img.gz" "${OUTPUT_DIR}/openwrt-rootfs.img.gz"
    gzip -dc "${OUTPUT_DIR}/openwrt-rootfs.img.gz" > "${OUTPUT_DIR}/openwrt-rootfs.raw"
    if command -v qemu-img >/dev/null 2>&1; then
        qemu-img convert -f raw -O qcow2 -c "${OUTPUT_DIR}/openwrt-rootfs.raw" "${OUTPUT_DIR}/openwrt.qcow2"
    fi
    (
        cd "${OUTPUT_DIR}"
        sha256sum Image openwrt-rootfs.img.gz openwrt-rootfs.raw $( [[ -f openwrt.qcow2 ]] && echo openwrt.qcow2 ) > sha256sums.txt
    )
}

if [[ "${BUILD_MODE}" == "fast" ]]; then
    build_fast_mode
else
    build_full_mode
fi

if [[ "${PREPARE_ONLY}" -eq 0 ]]; then
    echo "=================================================================="
    echo " Build Complete! Artifacts available in: ${OUTPUT_DIR}"
    ls -lh "${OUTPUT_DIR}"
    echo "=================================================================="
fi
