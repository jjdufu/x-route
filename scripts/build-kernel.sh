#!/usr/bin/env bash
# ==============================================================================
# x-route: 构建 Gunyah pVM guest 内核（scripts/build-kernel.sh）
#   Linux 6.12.103 + ImmortalWrt v25.12.2 的 generic/armsr 补丁
#   配置基线：ImmortalWrt 25.12.2 官方 armsr/armv8 内核的最终 .config（取自同版本 SDK）
#   再叠加 openwrt/kernel-gunyah-pvm.config 片段
# 产物：dist/openwrt/Image（arm64 未压缩内核）、dist/openwrt/kernel.config、
#       dist/openwrt/modules.builtin(.modinfo)（供 build-rootfs.sh 写入 rootfs，kmodloader 据此跳过内建模块）
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
  local cfg="$1" off="${2:-}" bad=0 k
  # Gunyah pVM 启动必需：必须内建
  for k in DMA_RESTRICTED_POOL SWIOTLB VIRTIO_PCI VIRTIO_BLK VIRTIO_NET VIRTIO_CONSOLE HVC_DRIVER \
           SERIAL_8250_CONSOLE VIRTIO_FS FUSE_FS EXT4_FS DEVTMPFS PACKET \
           IPV6 IP_ADVANCED_ROUTER IP_MULTIPLE_TABLES IPV6_MULTIPLE_TABLES IPV6_SUBTREES; do
    grep -qx "CONFIG_${k}=y" "$cfg" || { echo "MISSING CONFIG_${k}=y"; bad=1; }
  done
  # 路由器 / OpenVPN / Clash（OpenClash、nikki 等）全功能所需：内建或模块均可
  for k in TUN NF_CONNTRACK NF_CONNTRACK_MARK NF_CONNTRACK_EVENTS NF_CT_NETLINK NF_NAT \
           NF_TABLES NF_TABLES_INET NF_TABLES_IPV4 NF_TABLES_IPV6 NFT_CT NFT_NAT NFT_MASQ NFT_REDIR \
           NFT_REJECT NFT_REJECT_INET NFT_LOG NFT_LIMIT NFT_FIB_INET NFT_HASH NFT_NUMGEN NFT_QUEUE \
           NFT_TPROXY NFT_SOCKET NF_TPROXY_IPV4 NF_TPROXY_IPV6 NF_SOCKET_IPV4 NF_SOCKET_IPV6 \
           NF_FLOW_TABLE NFT_FLOW_OFFLOAD NETFILTER_XT_TARGET_TPROXY NETFILTER_XT_MATCH_SOCKET \
           INET_DIAG INET_TCP_DIAG INET_UDP_DIAG; do
    grep -qE "^CONFIG_${k}=(y|m)$" "$cfg" || { echo "MISSING CONFIG_${k}=y|m"; bad=1; }
  done
  for k in VIRTIO_BALLOON MODVERSIONS 9P_FS; do
    if grep -qE "^CONFIG_${k}=(y|m)" "$cfg"; then echo "FORBIDDEN CONFIG_${k}"; bad=1; fi
  done
  grep -q '^CONFIG_LOCALVERSION=""' "$cfg" || { echo "LOCALVERSION must be empty"; bad=1; }
  # 与官方内核保持一致：官方为 =m 的网络模块不得改成内建（否则官方 kmod 因依赖缺失无法 modprobe）
  if [ -n "$off" ] && [ -f "$off" ]; then
    # shellcheck disable=SC2013  # 选项名不含空白，按词读取即可
    for k in $(grep -oE '^CONFIG_(NETFILTER|NF|NFT|IP_NF|IP6_NF|IP_SET|BRIDGE_NF|NET_SCH|NET_CLS|NET_ACT|TUN|INET_DIAG|INET_TCP_DIAG|INET_UDP_DIAG|WIREGUARD)[A-Z0-9_]*=m' "$off" | cut -d= -f1); do
      if grep -qx "${k}=y" "$cfg"; then echo "BUILTIN-DIVERGES ${k}（官方为 m）"; bad=1; fi
    done
  fi
  [ "$bad" = 0 ]
}

if [ "${1:-}" = "--assert" ]; then assert_config "${2:?config path}" "${3:-}"; echo "assert OK"; exit 0; fi

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
assert_config .config ../official/config
make -s -j"${JOBS}" Image modules.builtin
grep -qx '6.12.103' include/config/kernel.release || { echo "unexpected kernel.release: $(cat include/config/kernel.release)"; exit 1; }
cp arch/arm64/boot/Image "${DIST}/Image"
cp .config "${DIST}/kernel.config"
cp modules.builtin modules.builtin.modinfo "${DIST}/"
echo "[+] ${DIST}/Image ($(du -h "${DIST}/Image" | cut -f1))"
