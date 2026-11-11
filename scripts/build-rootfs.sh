#!/usr/bin/env bash
# ==============================================================================
# XRouter: 用 ImmortalWrt 25.12.2 ImageBuilder 生成 VM 根文件系统（scripts/build-rootfs.sh）
# 产物：dist/openwrt/openwrt-rootfs.img.gz（不带分区表的 ext4，guest 中为 /dev/vda）
#       dist/openwrt/openwrt.qcow2（同一 rootfs 的 qcow2，便于在 PC 上用 QEMU 调试）
# 依赖：build-kernel.sh 生成的 dist/openwrt/modules.builtin(.modinfo)
# ==============================================================================
set -euo pipefail
export PATH="${PATH}:/usr/sbin:/sbin"   # debugfs / e2fsck

IMM=25.12.2
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${ROOT}/build/ib"
DIST="${ROOT}/dist/openwrt"
BASE="https://downloads.immortalwrt.org/releases/${IMM}/targets/armsr/armv8"
IB="immortalwrt-imagebuilder-${IMM}-armsr-armv8.Linux-x86_64"
BUILTIN="${DIST}/modules.builtin"   # 以及 modules.builtin.modinfo
mkdir -p "${WORK}" "${DIST}"
[ -f "${BUILTIN}" ] || { echo "missing ${BUILTIN}, run scripts/build-kernel.sh first"; exit 1; }

cd "${WORK}"
wget -q -N "${BASE}/sha256sums"
[ -f "${IB}.tar.zst" ] || wget -q "${BASE}/${IB}.tar.zst"
grep " \*${IB}.tar.zst\$" sha256sums | sed 's/ \*/  /' | sha256sum -c -
rm -rf "${IB}"
tar --zstd -xf "${IB}.tar.zst"
cd "${IB}"
# 只需要 ext4 根文件系统：关闭 squashfs / tar.gz / cpio / qcow2 / vmdk，缩短构建
sed -i -e 's/^CONFIG_TARGET_ROOTFS_SQUASHFS=y/# CONFIG_TARGET_ROOTFS_SQUASHFS is not set/' \
       -e 's/^CONFIG_TARGET_ROOTFS_TARGZ=y/# CONFIG_TARGET_ROOTFS_TARGZ is not set/' \
       -e 's/^CONFIG_TARGET_ROOTFS_CPIOGZ=y/# CONFIG_TARGET_ROOTFS_CPIOGZ is not set/' \
       -e 's/^CONFIG_QCOW2_IMAGES=y/# CONFIG_QCOW2_IMAGES is not set/' \
       -e 's/^CONFIG_VMDK_IMAGES=y/# CONFIG_VMDK_IMAGES is not set/' .config

# 只排除 ImageBuilder 默认包与 generic 设备包中的硬件驱动 kmod（网卡/存储/板级，pVM 里只有 virtio，已内建）；
# 网络功能 kmod（nft/nf/ipt/tun/diag 等）保留，与官方内核一致。
INFO="$(make info)"
DEF_KMODS="$( { echo "${INFO}" | sed -n 's/^Default Packages: //p'
               echo "${INFO}" | sed -n '/^generic:/,/^[^ ]/s/^ *Packages: //p'; } \
             | tr ' ' '\n' | grep '^kmod-' \
             | grep -vE '^kmod-(nft|nf-|ipt|ip6t|tun|inet-diag|fs-|crypto|lib-|ppp|wireguard|ovpn)' | sort -u | sed 's/^/-/' | tr '\n' ' ')"
PKGS="$(grep -vE '^[[:space:]]*(#|$)' "${ROOT}/openwrt/packages.txt" | tr '\n' ' ')"
echo "[*] excluded default kmods: ${DEF_KMODS}"

# OpenClash：打入 GitHub 官方 apk + 固定版本 linux-arm64 mihomo，首次开机安装，默认不启用。
# 版本与 sha256 写死在此，改动会改变镜像指纹，CI 会重建根文件系统。
OC_VER=0.48.002
OC_APK_SHA256=f7ebbd43f9be15c5e6df26e5d0c03d50d0bb8f7fa4cc0d2c2bf048b08c0b155b
MIHOMO_VER=v1.19.34
MIHOMO_SHA256=daaf78a6240a282cdfd75bcbf98fe321f6bd52765cc29e79f344191198f0b50f
fetch_sha() {
  dest="$1"; sha="$2"; shift 2
  url=""
  for url in "$@"; do
    echo "[*] fetching ${url}"
    if wget -q -O "${dest}" "${url}" && echo "${sha}  ${dest}" | sha256sum -c -; then
      return 0
    fi
    rm -f "${dest}"
  done
  echo "failed to fetch $(basename "${dest}")"
  exit 1
}
FILES_STAGE="${WORK}/files-stage"
rm -rf "${FILES_STAGE}"
mkdir -p "${FILES_STAGE}/usr/lib/xrouter"
cp -a "${ROOT}/openwrt/files/." "${FILES_STAGE}/"
fetch_sha "${FILES_STAGE}/usr/lib/xrouter/luci-app-openclash-${OC_VER}.apk" "${OC_APK_SHA256}" \
  "https://github.com/vernesong/OpenClash/releases/download/v${OC_VER}/luci-app-openclash-${OC_VER}.apk" \
  "https://dl.openclash.org/releases/latest/luci-app-openclash-${OC_VER}.apk"
fetch_sha "${WORK}/mihomo-linux-arm64-${MIHOMO_VER}.gz" "${MIHOMO_SHA256}" \
  "https://github.com/MetaCubeX/mihomo/releases/download/${MIHOMO_VER}/mihomo-linux-arm64-${MIHOMO_VER}.gz"
cp "${WORK}/mihomo-linux-arm64-${MIHOMO_VER}.gz" "${FILES_STAGE}/usr/lib/xrouter/clash_meta.gz"

make image PROFILE=generic \
  PACKAGES="${DEF_KMODS} ${PKGS}" \
  FILES="${FILES_STAGE}" \
  ROOTFS_PARTSIZE=512

# ImageBuilder 生成的原始 ext4 根文件系统（大小 = ROOTFS_PARTSIZE）
OUT="build_dir/target-aarch64_generic_musl/linux-armsr_armv8/root.ext4"
RAW="${WORK}/rootfs.raw"
cp "${OUT}" "${RAW}"
file "${RAW}" | grep -q "ext4 filesystem" || { echo "rootfs is not ext4"; exit 1; }

# 用自编内核的 modules.builtin 覆盖官方内核包里的同名文件，并删除已内建模块的官方 .ko：
# kmodloader 据此把内建功能视为“已加载”，不再加载官方同名 .ko（否则 duplicate symbol / 依赖探测失败）
KMOD_DIR="/lib/modules/6.12.103"
for f in modules.builtin modules.builtin.modinfo; do
  debugfs -w -R "rm ${KMOD_DIR}/${f}" "${RAW}" >/dev/null 2>&1 || true
  debugfs -w -R "write ${DIST}/${f} ${KMOD_DIR}/${f}" "${RAW}" >/dev/null 2>&1
  debugfs -w -R "sif ${KMOD_DIR}/${f} mode 0100644" "${RAW}" >/dev/null 2>&1
  debugfs -w -R "sif ${KMOD_DIR}/${f} uid 0" "${RAW}" >/dev/null 2>&1
  debugfs -w -R "sif ${KMOD_DIR}/${f} gid 0" "${RAW}" >/dev/null 2>&1
done
builtin_names="$(sed 's#.*/##; s#\.ko$##' "${BUILTIN}" | tr '-' '_' | sort -u)"
for ko in $(debugfs -R "ls -p ${KMOD_DIR}" "${RAW}" 2>/dev/null | awk -F/ '$6 ~ /\.ko$/ {print $6}'); do
  n="$(echo "${ko%.ko}" | tr '-' '_')"
  if echo "${builtin_names}" | grep -qx "${n}"; then
    debugfs -w -R "rm ${KMOD_DIR}/${ko}" "${RAW}" >/dev/null 2>&1
    echo "[*] removed builtin module ${ko}"
  fi
done
# 依赖内建模块的官方 .ko（例如 ImmortalWrt 的 nft_fullcone）：kmodloader 只认 .ko 之间的依赖，
# 无法加载这类模块，开机时只会报错，一并删除
tmpko="$(mktemp --suffix=.ko)"   # modinfo 只接受 .ko 后缀的路径
for ko in $(debugfs -R "ls -p ${KMOD_DIR}" "${RAW}" 2>/dev/null | awk -F/ '$6 ~ /\.ko$/ {print $6}'); do
  debugfs -R "dump ${KMOD_DIR}/${ko} ${tmpko}" "${RAW}" >/dev/null 2>&1
  for d in $(modinfo -F depends "${tmpko}" 2>/dev/null | tr ',' ' '); do
    if echo "${builtin_names}" | grep -qx "$(echo "$d" | tr '-' '_')"; then
      debugfs -w -R "rm ${KMOD_DIR}/${ko}" "${RAW}" >/dev/null 2>&1
      echo "[*] removed ${ko} (depends on builtin ${d})"
      break
    fi
  done
done
rm -f "${tmpko}"
cmp <(debugfs -R "cat ${KMOD_DIR}/modules.builtin" "${RAW}" 2>/dev/null) "${DIST}/modules.builtin" \
  || { echo "modules.builtin not replaced"; exit 1; }

# make_ext4fs 生成的 resize_inode 布局不支持在线扩容（resize2fs 报 reserve_backup_gdb 错误）。
# 去掉 resize_inode 后，内核在线扩容会自动转换为 meta_bg，VM 开机时 resize2fs /dev/vda 可直接撑满。
tune2fs -O ^resize_inode "${RAW}" >/dev/null
e2fsck -fy "${RAW}" >/dev/null 2>&1 || [ $? -le 1 ]

# 检查：关键文件存在，禁止的文件不存在，只有 apk 没有 opkg
for f in /usr/sbin/openvpn /usr/bin/openssl /usr/bin/tcpdump /usr/bin/ttyd /usr/bin/apk /lib/apk/db/installed \
         /etc/uci-defaults/10-xrouter /etc/uci-defaults/30-xrouter-openclash \
         /usr/lib/xrouter/luci-app-openclash-0.48.002.apk /usr/lib/xrouter/clash_meta.gz \
         /bin/bash /usr/bin/ruby /usr/bin/unzip \
         /etc/init.d/xrouter-net /etc/init.d/xrouter-ready /etc/init.d/xrouter-bootlog /etc/init.d/xrouter-share /etc/init.d/xrouter-vpn /etc/openvpn/xrouter-up.sh /etc/openvpn/xrouter-down.sh /etc/board.d/99-xrouter-model /usr/libexec/xrouter-ipv6 /usr/libexec/xrouter-diag /usr/libexec/xrouter-hotspot /usr/libexec/xrouter-mtu; do
  debugfs -R "stat ${f}" "${RAW}" 2>/dev/null | grep -q "Inode:" || { echo "missing ${f}"; exit 1; }
done
for f in /usr/bin/htop /usr/bin/owut /usr/lib/opkg /bin/opkg /usr/bin/easyrsa; do
  if debugfs -R "stat ${f}" "${RAW}" 2>/dev/null | grep -q "Inode:"; then echo "forbidden ${f}"; exit 1; fi
done
for f in /etc/rc.d/S11xrouter-net /etc/rc.d/S13xrouter-bootlog /etc/rc.d/S99xrouter-ready; do
  debugfs -R "stat ${f}" "${RAW}" 2>/dev/null | grep -q "Inode:" || { echo "${f} not enabled"; exit 1; }
done
if debugfs -R "cat /etc/inittab" "${RAW}" 2>/dev/null | grep -q '^hvc0'; then echo "inittab still has hvc0"; exit 1; fi
for f in /etc/uci-defaults/10-xrouter /etc/uci-defaults/30-xrouter-openclash /etc/init.d/xrouter-net /etc/init.d/xrouter-ready /etc/init.d/xrouter-bootlog /etc/init.d/xrouter-share /etc/init.d/xrouter-vpn /etc/openvpn/xrouter-up.sh /etc/openvpn/xrouter-down.sh /etc/board.d/99-xrouter-model /usr/libexec/xrouter-ipv6 /usr/libexec/xrouter-diag /usr/libexec/xrouter-hotspot /usr/libexec/xrouter-mtu; do
  debugfs -R "stat ${f}" "${RAW}" 2>/dev/null | grep -q "Mode:  0755" || { echo "${f} not 0755"; exit 1; }
done

gzip -9n -c "${RAW}" > "${DIST}/openwrt-rootfs.img.gz"
if command -v qemu-img >/dev/null 2>&1; then
  qemu-img convert -f raw -O qcow2 -c "${RAW}" "${DIST}/openwrt.qcow2"
fi
cp "bin/targets/armsr/armv8/immortalwrt-${IMM}-armsr-armv8-generic.manifest" "${DIST}/openwrt.manifest" 2>/dev/null || true
if [ -f "${DIST}/openwrt.manifest" ]; then
  grep -q 'luci-i18n-package-manager-zh-cn' "${DIST}/openwrt.manifest" || { echo "missing luci-i18n-package-manager-zh-cn"; exit 1; }
fi
rm -f "${RAW}"
echo "[+] ${DIST}/openwrt-rootfs.img.gz ($(du -h "${DIST}/openwrt-rootfs.img.gz" | cut -f1))"
