# 04 固件：ImmortalWrt 25.12.2 ImageBuilder

产物：`dist/openwrt-rootfs.raw.gz`，是不带分区表的 ext4 根文件系统，guest 里是 `/dev/vda`。

## 软件包清单 `openwrt/packages.txt`

一行一个包名，`#` 开头为注释，`-` 前缀表示排除。

```
# 基础
dnsmasq
firewall4
nftables
dropbear
ca-bundle
# LuCI
luci
luci-i18n-base-zh-cn
luci-i18n-firewall-zh-cn
ttyd
luci-app-ttyd
luci-i18n-ttyd-zh-cn
# 调试
tcpdump
ip-full
ip-bridge
ss
ethtool
mtr-nojson
curl
bind-dig
iperf3
conntrack
resize2fs
# OpenVPN 客户端（显式列出依赖，ImageBuilder 也会自动解析）
openvpn-openssl
kmod-tun
kmod-ovpn-backports
libcap-ng
liblzo2
liblz4-1
libnl-genl200
libopenssl3
openssl-util
luci-app-openvpn
luci-i18n-openvpn-zh-cn
luci-compat
# 排除
-luci-app-attendedsysupgrade
-owut
-htop
```

不装：`openvpn-easy-rsa`、`luci-app-openvpn-server`、任何 OpenList 相关包。

`kmod-tun` 和 `kmod-ovpn-backports` 是官方内核的模块，装进镜像但不会被加载：tun 已经直接编进内核，ovpn 加载失败时 openvpn 会自动退回 tun 模式。它们只是为了满足 apk 的依赖关系。

## `scripts/build-rootfs.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail
IMM=25.12.2
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${ROOT}/build/ib"; DIST="${ROOT}/dist"; mkdir -p "${WORK}" "${DIST}"
BASE="https://downloads.immortalwrt.org/releases/${IMM}/targets/armsr/armv8"
IB="immortalwrt-imagebuilder-${IMM}-armsr-armv8.Linux-x86_64"
cd "${WORK}"
wget -q -N "${BASE}/sha256sums" "${BASE}/${IB}.tar.zst"
grep " \*\?${IB}.tar.zst\$" sha256sums | sha256sum -c -
rm -rf "${IB}"; tar --zstd -xf "${IB}.tar.zst"; cd "${IB}"

# 排除 ImageBuilder 默认带的全部 kmod（官方内核模块，在自编内核上无用）
DEF_KMODS="$(make info | sed -n 's/^Default Packages: //p' | tr ' ' '\n' | grep '^kmod-' | sed 's/^/-/' | tr '\n' ' ')"
PKGS="$(grep -vE '^\s*(#|$)' "${ROOT}/openwrt/packages.txt" | tr '\n' ' ')"

make image PROFILE=generic \
  PACKAGES="${DEF_KMODS} ${PKGS}" \
  FILES="${ROOT}/openwrt/files" \
  ROOTFS_PARTSIZE=256 \
  DISABLED_SERVICES="" 

OUT="bin/targets/armsr/armv8/immortalwrt-${IMM}-armsr-armv8-generic-ext4-rootfs.img.gz"
cp "${OUT}" "${DIST}/openwrt-rootfs.raw.gz"
# 检查：解压后应是 ext4，并且包含关键文件
zcat "${DIST}/openwrt-rootfs.raw.gz" > "${WORK}/check.raw"
file "${WORK}/check.raw" | grep -q "ext4 filesystem"
for f in /usr/sbin/openvpn /usr/bin/openssl /usr/sbin/tcpdump /etc/uci-defaults/10-x-route /etc/init.d/x-route-ready; do
  debugfs -R "stat ${f}" "${WORK}/check.raw" >/dev/null 2>&1 || { echo "missing ${f}"; exit 1; }
done
for f in /usr/bin/htop /usr/bin/owut; do
  debugfs -R "stat ${f}" "${WORK}/check.raw" >/dev/null 2>&1 && { echo "forbidden ${f}"; exit 1; }
done
rm -f "${WORK}/check.raw"
```

`ROOTFS_PARTSIZE=256` 只是出厂镜像的大小。安装时由 `customize.sh` 用 `truncate` 扩到 `VM_DISK_SIZE_MB`，VM 每次开机由 `x-route-net` 执行 `resize2fs /dev/vda` 撑满。

## `openwrt/files/etc/inittab`

覆盖 armsr 默认的 inittab。默认文件会在 `hvc0` 上起登录终端，和 05、06 用作就绪信号通道的 `hvc0` 冲突。

```
::sysinit:/etc/init.d/rcS S boot
::shutdown:/etc/init.d/rcS K shutdown
ttyS0::askfirst:/usr/libexec/login.sh
```

## `openwrt/files/etc/uci-defaults/10-x-route`（首次开机执行一次）

```sh
#!/bin/sh
# root 密码、ttyd、dropbear 全部保持上游默认（root 空密码），不做任何修改

# 1. 系统
uci -q batch <<'U'
set system.@system[0].hostname='x-route'
set luci.main.lang='zh_cn'
commit system
commit luci
U

# 2. 网络：单网卡 eth0，静态地址；地址每次开机由 x-route-net 按 cmdline 写入
uci -q batch <<'U'
delete network.wan
delete network.wan6
delete network.@device[0]
set network.lan=interface
set network.lan.proto='static'
set network.lan.device='eth0'
set network.lan.netmask='255.255.255.0'
delete network.lan.ip6assign
set network.vpn=interface
set network.vpn.proto='none'
set network.vpn.device='tun0'
commit network
set dhcp.lan.ignore='1'
delete dhcp.wan
commit dhcp
U

# 3. 防火墙：只保留 lan 和 vpn 两个分区
while uci -q delete firewall.@zone[0]; do :; done
while uci -q delete firewall.@forwarding[0]; do :; done
uci -q batch <<'U'
add firewall zone
set firewall.@zone[-1].name='lan'
add_list firewall.@zone[-1].network='lan'
set firewall.@zone[-1].input='ACCEPT'
set firewall.@zone[-1].output='ACCEPT'
set firewall.@zone[-1].forward='ACCEPT'
add firewall zone
set firewall.@zone[-1].name='vpn'
add_list firewall.@zone[-1].network='vpn'
add_list firewall.@zone[-1].device='tun+'
set firewall.@zone[-1].input='REJECT'
set firewall.@zone[-1].output='ACCEPT'
set firewall.@zone[-1].forward='REJECT'
set firewall.@zone[-1].masq='1'
set firewall.@zone[-1].mtu_fix='1'
add firewall forwarding
set firewall.@forwarding[-1].src='lan'
set firewall.@forwarding[-1].dest='vpn'
commit firewall
U

exit 0
```

`firewall.@rule` 中 fw4 默认那些针对 wan 的规则（Allow-DHCP-Renew 等）在 wan 分区删除后不再生效，保持原样即可，fw4 会跳过分区不存在的规则。如果 `fw4 check` 报错，就在第 3 段里加上 `while uci -q delete firewall.@rule[0]; do :; done`。

## `openwrt/files/etc/init.d/x-route-net`（每次开机执行）

```sh
#!/bin/sh /etc/rc.common
START=11
cmdval() { sed -n "s/.*[[:space:]]$1=\([^[:space:]]*\).*/\1/p" /proc/cmdline; }
boot() { start; }
start() {
  ip="$(cmdval xr.ip)"; gw="$(cmdval xr.gw)"; dns="$(cmdval xr.dns | tr ',' ' ')"; mode="$(cmdval xr.vpnmode)"
  resize2fs /dev/vda >/dev/null 2>&1   # 每次开机执行；已撑满时是空操作，配合 xr expand
  [ -n "$ip" ] || return 0
  addr="${ip%/*}"; net="${addr%.*}"
  uci -q batch <<U
set network.lan.ipaddr='${addr}'
set network.lan.gateway='${gw}'
delete network.lan.dns
U
  for d in $dns; do uci -q add_list network.lan.dns="$d"; done
  z="$(uci show firewall | sed -n "s/^firewall\.\(@zone\[[0-9]*\]\)\.name='vpn'$/\1/p")"
  uci -q delete "firewall.${z}.masq_src"
  f="$(uci show firewall | grep -n "src='vpn'" | head -1)"
  if [ "$mode" = "site" ]; then
    uci -q add_list "firewall.${z}.masq_src=!${net}.0/24"
    [ -n "$f" ] || { uci -q add firewall forwarding; uci -q set firewall.@forwarding[-1].src='vpn'; uci -q set firewall.@forwarding[-1].dest='lan'; }
  else
    while i="$(uci show firewall | sed -n "s/^firewall\.\(@forwarding\[[0-9]*\]\)\.src='vpn'$/\1/p" | head -1)"; [ -n "$i" ]; do uci -q delete "firewall.${i}"; done
  fi
  uci -q commit network; uci -q commit firewall
}
```

`START=11` 早于 network（20）和 firewall（19），所以不需要 reload。

## `openwrt/files/etc/init.d/x-route-ready`

```sh
#!/bin/sh /etc/rc.common
START=99
boot() { start; }
start() {
  msg="XR_READY $(cut -d' ' -f1 /proc/uptime)"
  for d in /dev/hvc0 /dev/ttyS0; do
    [ -c "$d" ] && { echo "$msg" > "$d"; [ "$d" = /dev/hvc0 ] && break; }
  done
}
```

先写 `hvc0`，也就是 crosvm 的第二个控制台通道（virtio-console），手机侧对应 `run/vm-ctl.log`。`hvc0` 不存在时退回 `ttyS0`，对应手机侧 `run/console.log`。

## `openwrt/files/etc/init.d/x-route-share`

```sh
#!/bin/sh /etc/rc.common
START=21
boot() { start; }
start() {
  mode="$(sed -n 's/.*[[:space:]]xr.share=\([a-z]*\).*/\1/p' /proc/cmdline)"
  case "$mode" in ro|rw) ;; *) return 0 ;; esac
  grep -qw virtiofs /proc/filesystems || return 0
  mkdir -p /mnt/sdcard
  mountpoint -q /mnt/sdcard || mount -t virtiofs -o "$mode" xrshare /mnt/sdcard
}
stop() { umount /mnt/sdcard 2>/dev/null; return 0; }
```

三个 init 脚本都要 `chmod 0755`。ImageBuilder 的 `FILES` 会保留文件权限，所以要在仓库里设好可执行位：`git update-index --chmod=+x`。

## 验收

- `bash scripts/build-rootfs.sh` 通过（含脚本里的检查），生成 `dist/openwrt-rootfs.raw.gz`。
- 用 `debugfs -R "cat /etc/inittab"` 查看，没有 `hvc0` 一行。
- 用 `debugfs -R "ls /usr/lib/opkg"` 查看，应不存在；`/lib/apk/db/installed` 应存在。
- 镜像里没有 `htop`、`owut`、`luci-app-attendedsysupgrade`、`openvpn-easy-rsa`、openlist。
