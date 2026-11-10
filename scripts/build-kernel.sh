#!/usr/bin/env bash
# ==============================================================================
# x-route: 构建 Gunyah pVM guest 内核（scripts/build-kernel.sh）
#   Linux 6.12.103 + ImmortalWrt v25.12.2 的 generic/armsr 补丁
#   配置基线：ImmortalWrt 25.12.2 官方 armsr/armv8 内核的最终 .config（取自同版本 SDK）
#   再叠加 openwrt/kernel-gunyah-pvm.config 片段
# 产物：dist/openwrt/Image（arm64 未压缩内核）、dist/openwrt/kernel.config、
#       dist/openwrt/modules.builtin（供 build-rootfs.sh 去掉内建模块的自动加载项）
# ==============================================================================
set -euo pipefail

KVER=6.12.103
IMM_TAG=v25.12.2
IMM=25.12.2
BASE="https://downloads.immortalwrt.org/releases/${IMM}/targets/armsr/armv8"
SDK="immortalwrt-sdk-${IMM}-armsr-armv8_gcc-14.3.0_musl.Linux-x86_64.tar.zst"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${ROOT}/build/kernel"
DIST="${ROOT}/dist/openwrt"
JOBS="${JOBS:-$(nproc)}"

assert_config() {
  local cfg="$1" bad=0 k
  for k in DMA_RESTRICTED_POOL SWIOTLB VIRTIO_PCI VIRTIO_BLK VIRTIO_NET VIRTIO_CONSOLE HVC_DRIVER \
           SERIAL_8250_CONSOLE VIRTIO_FS FUSE_FS TUN IP_ADVANCED_ROUTER IP_MULTIPLE_TABLES \
           NF_TABLES NF_TABLES_INET NFT_CT NFT_NAT NFT_MASQ NFT_REJECT NFT_REJECT_INET \
           NFT_LOG NFT_LIMIT NFT_FIB_INET INET_DIAG PACKET EXT4_FS DEVTMPFS; do
    grep -qx "CONFIG_${k}=y" "$cfg" || { echo "MISSING CONFIG_${k}=y"; bad=1; }
  done
  for k in VIRTIO_BALLOON MODVERSIONS 9P_FS; do
    if grep -qE "^CONFIG_${k}=(y|m)" "$cfg"; then echo "FORBIDDEN CONFIG_${k}"; bad=1; fi
  done
  grep -q '^CONFIG_LOCALVERSION=""' "$cfg" || { echo "LOCALVERSION must be empty"; bad=1; }
  [ "$bad" = 0 ]
}

if [ "${1:-}" = "--assert" ]; then assert_config "${2:?config path}"; echo "assert OK"; exit 0; fi

mkdir -p "${WORK}" "${DIST}"
cd "${WORK}"

if [ ! -d imw ]; then
  git clone -q --depth 1 --branch "${IMM_TAG}" --filter=blob:none --sparse \
    https://github.com/immortalwrt/immortalwrt.git imw
  git -C imw sparse-checkout set target/linux/generic target/linux/armsr
fi
HASH="$(sed -n "s/^LINUX_KERNEL_HASH-${KVER} = //p" imw/target/linux/generic/kernel-6.12)"
[ -n "${HASH}" ] || { echo "kernel hash for ${KVER} not found in ${IMM_TAG}"; exit 1; }

# 官方内核最终配置（SDK 中 build_dir/.../linux-6.12.103/.config），sha256 由官方 sha256sums 校验
if [ ! -f official/config ]; then
  wget -q -N "${BASE}/sha256sums"
  [ -f "${SDK}" ] || wget -q "${BASE}/${SDK}"
  grep " \*${SDK}\$" sha256sums | sed 's/ \*/  /' | sha256sum -c -
  mkdir -p official
  tar --zstd -xf "${SDK}" -C official --wildcards --strip-components=4 \
    "*/linux-${KVER}/.config" "*/linux-${KVER}/Module.symvers"
  mv "official/linux-${KVER}/.config" official/config
  mv "official/linux-${KVER}/Module.symvers" official/Module.symvers
  rm -rf "official/linux-${KVER}" "${SDK}"
fi

[ -f "linux-${KVER}.tar.xz" ] || wget -q "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KVER}.tar.xz"
echo "${HASH}  linux-${KVER}.tar.xz" | sha256sum -c -
rm -rf "linux-${KVER}"
tar xf "linux-${KVER}.tar.xz"
cd "linux-${KVER}"

# 与 OpenWrt 构建顺序一致：generic/files -> generic 补丁 -> 目标补丁
cp -a ../imw/target/linux/generic/files/. .
for d in backport-6.12 pending-6.12 hack-6.12; do
  for p in $(find "../imw/target/linux/generic/${d}" -maxdepth 1 -name '*.patch' | sort); do
    patch -p1 -s --forward --no-backup-if-mismatch < "$p" || { echo "patch failed: ${p}"; exit 1; }
  done
done
for p in $(find ../imw/target/linux/armsr/patches-6.12 -maxdepth 1 -name '*.patch' 2>/dev/null | sort); do
  patch -p1 -s --forward --no-backup-if-mismatch < "$p" || { echo "patch failed: ${p}"; exit 1; }
done

# 后面的配置覆盖前面的：官方最终配置 -> Gunyah 片段
cat ../official/config "${ROOT}/openwrt/kernel-gunyah-pvm.config" > .config

export ARCH=arm64 CROSS_COMPILE="${CROSS_COMPILE:-aarch64-linux-gnu-}"
export KBUILD_BUILD_USER=x-route KBUILD_BUILD_HOST=github-actions
make -s olddefconfig
assert_config .config
make -s -j"${JOBS}" Image modules.builtin
grep -qx '6.12.103' include/config/kernel.release || { echo "unexpected kernel.release: $(cat include/config/kernel.release)"; exit 1; }
cp arch/arm64/boot/Image "${DIST}/Image"
cp .config "${DIST}/kernel.config"
cp modules.builtin "${DIST}/modules.builtin"
echo "[+] ${DIST}/Image ($(du -h "${DIST}/Image" | cut -f1))"
