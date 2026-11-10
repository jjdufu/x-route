# 03 内核：6.12.103 + 25.12 armsr 配置 + Gunyah pVM 片段

产物：`dist/Image`，即 arm64 未压缩内核，crosvm 直接加载。

## 来源

- 内核源码：kernel.org 的 `linux-6.12.103.tar.xz`。sha256 来自 ImmortalWrt `v25.12.2` 的 `target/linux/generic/kernel-6.12`（`LINUX_KERNEL_HASH-6.12.103`）。
- 补丁和配置：ImmortalWrt `v25.12.2` 用稀疏检出获取以下内容：
  - `target/linux/generic/files/`：复制进内核源码树，与 OpenWrt 的构建方式一致；
  - `target/linux/generic/{backport,pending,hack}-6.12/*.patch`：按目录顺序、文件名排序后应用；
  - `target/linux/armsr/patches-6.12/*.patch`；
  - 配置：`target/linux/generic/config-6.12`、`target/linux/armsr/config-6.12`、`target/linux/armsr/armv8/config-6.12`。
- 打上 OpenWrt 补丁是为了让结构体布局尽量接近官方 kmod，提高 `apk add kmod-*` 后能加载的概率。不能保证，见 13。

## `scripts/build-kernel.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail
KVER=6.12.103
IMM_TAG=v25.12.2
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${ROOT}/build/kernel"; DIST="${ROOT}/dist"
mkdir -p "${WORK}" "${DIST}"
cd "${WORK}"

[ -d imw ] || { git clone -q --depth 1 --branch "${IMM_TAG}" --filter=blob:none --sparse \
    https://github.com/immortalwrt/immortalwrt.git imw
  git -C imw sparse-checkout set target/linux/generic target/linux/armsr; }
HASH="$(sed -n "s/^LINUX_KERNEL_HASH-${KVER} = //p" imw/target/linux/generic/kernel-6.12)"

[ -f "linux-${KVER}.tar.xz" ] || wget -q "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KVER}.tar.xz"
echo "${HASH}  linux-${KVER}.tar.xz" | sha256sum -c -
rm -rf "linux-${KVER}"; tar xf "linux-${KVER}.tar.xz"; cd "linux-${KVER}"

cp -a ../imw/target/linux/generic/files/. .
for d in backport-6.12 pending-6.12 hack-6.12; do
  for p in $(ls ../imw/target/linux/generic/${d}/*.patch | sort); do patch -p1 -s --forward < "$p"; done
done
for p in $(ls ../imw/target/linux/armsr/patches-6.12/*.patch 2>/dev/null | sort); do patch -p1 -s --forward < "$p"; done

cat ../imw/target/linux/generic/config-6.12 \
    ../imw/target/linux/armsr/config-6.12 \
    ../imw/target/linux/armsr/armv8/config-6.12 \
    "${ROOT}/openwrt/kernel-gunyah-pvm.config" > .config
export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
make olddefconfig
assert_config .config   # 函数定义见下文“断言”，放在脚本顶部
make -j"$(nproc)" Image
cp arch/arm64/boot/Image "${DIST}/Image"
cp .config "${DIST}/kernel.config"
```

说明：
- 后面的配置覆盖前面的，所以 Gunyah 片段放在最后。
- 补丁应用失败直接退出（`set -e`）。不跳过任何补丁；有冲突就在这一步修正，并记录到 13。
- `assert_config()` 定义在脚本顶部，在 `olddefconfig` 之后、`make Image` 之前调用。

## `openwrt/kernel-gunyah-pvm.config`（整体重写）

```
# --- Gunyah pVM 必需 ---
CONFIG_DMA_RESTRICTED_POOL=y
CONFIG_SWIOTLB=y
CONFIG_PCI=y
CONFIG_PCI_HOST_GENERIC=y
CONFIG_VIRTIO=y
CONFIG_VIRTIO_MENU=y
CONFIG_VIRTIO_PCI=y
CONFIG_VIRTIO_MMIO=y
CONFIG_VIRTIO_BLK=y
CONFIG_VIRTIO_NET=y
CONFIG_VIRTIO_CONSOLE=y
CONFIG_HVC_DRIVER=y
CONFIG_SERIAL_8250=y
CONFIG_SERIAL_8250_CONSOLE=y
CONFIG_SERIAL_OF_PLATFORM=y
CONFIG_SERIAL_AMBA_PL011=y
CONFIG_SERIAL_AMBA_PL011_CONSOLE=y
CONFIG_SERIAL_EARLYCON=y
CONFIG_ARM_PSCI_FW=y
CONFIG_ARM_GIC_V3=y
CONFIG_DEVTMPFS=y
CONFIG_DEVTMPFS_MOUNT=y
CONFIG_EXT4_FS=y
# CONFIG_VIRTIO_BALLOON is not set
# --- 共享目录 ---
CONFIG_FUSE_FS=y
CONFIG_VIRTIO_FS=y
# --- VPN ---
CONFIG_TUN=y
# --- 路由 ---
CONFIG_IP_ADVANCED_ROUTER=y
CONFIG_IP_MULTIPLE_TABLES=y
CONFIG_IPV6_MULTIPLE_TABLES=y
# --- nftables (fw4) ---
CONFIG_NETFILTER=y
CONFIG_NF_CONNTRACK=y
CONFIG_NF_NAT=y
CONFIG_NF_TABLES=y
CONFIG_NF_TABLES_INET=y
CONFIG_NF_TABLES_IPV4=y
CONFIG_NF_TABLES_IPV6=y
CONFIG_NFT_CT=y
CONFIG_NFT_NAT=y
CONFIG_NFT_MASQ=y
CONFIG_NFT_REDIR=y
CONFIG_NFT_EXTHDR=y
CONFIG_NFT_RT=y
CONFIG_NFT_REJECT=y
CONFIG_NFT_REJECT_INET=y
CONFIG_NFT_LOG=y
CONFIG_NFT_LIMIT=y
CONFIG_NFT_QUOTA=y
CONFIG_NFT_FIB=y
CONFIG_NFT_FIB_INET=y
CONFIG_NFT_FIB_IPV4=y
CONFIG_NFT_FIB_IPV6=y
CONFIG_NF_LOG_SYSLOG=y
# --- 诊断 ---
CONFIG_PACKET=y
CONFIG_INET_DIAG=y
CONFIG_INET_TCP_DIAG=y
CONFIG_INET_UDP_DIAG=y
# --- 不需要 ---
# CONFIG_NET_9P is not set
# CONFIG_9P_FS is not set
# CONFIG_OVERLAY_FS is not set
# CONFIG_MODVERSIONS is not set
CONFIG_LOCALVERSION=""
# CONFIG_LOCALVERSION_AUTO is not set
```

## 断言 `assert_config()`

```bash
assert_config() {
  local cfg="$1" bad=0
  for k in DMA_RESTRICTED_POOL SWIOTLB VIRTIO_PCI VIRTIO_BLK VIRTIO_NET VIRTIO_CONSOLE HVC_DRIVER \
           SERIAL_8250_CONSOLE VIRTIO_FS FUSE_FS TUN IP_ADVANCED_ROUTER IP_MULTIPLE_TABLES \
           NF_TABLES NF_TABLES_INET NFT_CT NFT_NAT NFT_MASQ NFT_EXTHDR NFT_RT NFT_REJECT NFT_REJECT_INET \
           NFT_LOG NFT_LIMIT NFT_FIB_INET INET_DIAG PACKET EXT4_FS DEVTMPFS; do
    grep -qx "CONFIG_${k}=y" "$cfg" || { echo "MISSING CONFIG_${k}=y"; bad=1; }
  done
  for k in VIRTIO_BALLOON MODVERSIONS 9P_FS; do
    grep -qE "^CONFIG_${k}=(y|m)" "$cfg" && { echo "FORBIDDEN CONFIG_${k}"; bad=1; }
  done
  grep -q '^CONFIG_LOCALVERSION=""' "$cfg" || { echo "LOCALVERSION must be empty"; bad=1; }
  [ "$bad" = 0 ]
}
```

## 验收

- `bash scripts/build-kernel.sh` 不报错，并生成 `dist/Image` 和 `dist/kernel.config`。
- `assert_config dist/kernel.config` 通过。
- `strings dist/Image | grep -m1 "Linux version 6.12.103"` 有输出。
- `file dist/Image` 显示 `Linux kernel ARM64 boot executable Image`。
