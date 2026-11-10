# 05 模块骨架

## `module/module.prop`

```
id=x_route
name=x-route
version=v2.0.0-rc1
versionCode=200
author=jjdufu
description=Gunyah pVM 运行 ImmortalWrt 25.12.2。默认不启动，在 WebUI 中手动启动。
```

不在运行时修改 module.prop。删除 `ensure_module_prop` 相关逻辑。

## `module/config/config.conf`（默认配置，全量）

```sh
# 网络
XR_NET_PREFIX="172.29.233"
XR_VM_MAC="52:54:00:1d:e9:fe"
XR_DNS="223.5.5.5 119.29.29.29"
XR_EXPOSE="hotspot"
# VM
VM_CPUS="2"
VM_MEM_MB="512"
VM_SWIOTLB_MB="64"
VM_NET="1"
VM_DISK_RW="1"
VM_DISK_SIZE_MB="1024"
VM_WAKELOCK="1"
# 共享
VM_SHARE="0"
VM_SHARE_MODE="ro"
VM_SHARE_PATH="/data/media/0"
# OpenVPN
VPN_MODE="terminal"
VPN_ROUTES=""
# 启动与熔断
AUTOSTART="0"
AUTOSTART_DELAY_SEC="60"
READY_TIMEOUT_SEC="180"
STABLE_SEC="180"
```

## 键的取值约束

`common.sh` 的 `validate_kv` 和 WebUI 共用这一张表。

| 键 | 校验 |
|---|---|
| `XR_NET_PREFIX` | `^(10\|172\.(1[6-9]\|2[0-9]\|3[01])\|192\.168)\.[0-9]{1,3}(\.[0-9]{1,3})?$`，规范化后必须是三段 |
| `XR_VM_MAC` | `^52:54:00(:[0-9a-f]{2}){3}$` |
| `XR_DNS` | 1–3 个 IPv4 地址，空格分隔 |
| `XR_EXPOSE` | `off\|hotspot\|all` |
| `VM_CPUS` | 1–4 |
| `VM_MEM_MB` | 384–2048 |
| `VM_SWIOTLB_MB` | `32\|64\|128` |
| `VM_NET`、`VM_DISK_RW`、`VM_WAKELOCK`、`VM_SHARE`、`AUTOSTART` | `0\|1` |
| `VM_DISK_SIZE_MB` | 512–16384，只能增大 |
| `VM_SHARE_MODE` | `ro\|rw` |
| `VM_SHARE_PATH` | 必须以 `/data/media/0` 开头，不含 `..`，目录必须存在 |
| `VPN_MODE` | `terminal\|site` |
| `VPN_ROUTES` | 逗号分隔的 IPv4 CIDR，最多 64 条，校验见 08 |
| `AUTOSTART_DELAY_SEC` | 0–600 |
| `READY_TIMEOUT_SEC`、`STABLE_SEC` | 60–900 |

## `module/scripts/common.sh`（重写）

```sh
#!/system/bin/sh
MODDIR="${MODDIR:-/data/adb/modules/x_route}"
XR_DATA="/data/adb/x-route"
CONF="${XR_DATA}/config/config.conf"
RUN="${XR_DATA}/run"
LOG="${XR_DATA}/log/xr.log"
IMG="${XR_DATA}/images"
FUSE="${XR_DATA}/config/vm_booting.fuse"
TRIPPED="${XR_DATA}/config/tripped"
LAST_GOOD="${XR_DATA}/config/last_good"
KEYS="XR_NET_PREFIX XR_VM_MAC XR_DNS XR_EXPOSE VM_CPUS VM_MEM_MB VM_SWIOTLB_MB VM_NET VM_DISK_RW VM_DISK_SIZE_MB VM_WAKELOCK VM_SHARE VM_SHARE_MODE VM_SHARE_PATH VPN_MODE VPN_ROUTES AUTOSTART AUTOSTART_DELAY_SEC READY_TIMEOUT_SEC STABLE_SEC"
# 影响 VM 稳定性的键：只要这些键变化，last_good 就失效
HASH_KEYS="XR_NET_PREFIX VM_CPUS VM_MEM_MB VM_SWIOTLB_MB VM_NET VM_DISK_RW VM_SHARE VM_SHARE_MODE"

ensure_dirs()  { mkdir -p "${XR_DATA}/config" "${RUN}" "${XR_DATA}/log" "${IMG}"; }
load_config()  { . "${CONF}"; }
log_msg()      { # $1=级别 $2=模块 $3...=消息
  ensure_dirs
  [ -f "${LOG}" ] && [ "$(stat -c %s "${LOG}")" -gt 524288 ] && mv -f "${LOG}" "${LOG}.1"
  lv="$1"; m="$2"; shift 2
  echo "$(date '+%F %T') [${lv}] [${m}] $*" >> "${LOG}"
}
log_info() { log_msg INFO "$@"; }
log_err()  { log_msg ERROR "$@"; }

valid_kv() { ... }        # 按上面的约束表逐键 case 校验，失败返回 1
set_kv() {                # $1=键 $2=值：先校验，写临时文件，mv 替换，再 sync
  valid_kv "$1" "$2" || return 1
  tmp="${CONF}.tmp.$$"
  awk -v k="$1" -v v="$2" 'BEGIN{d=0} $0 ~ "^"k"=" {print k"=\""v"\""; d=1; next} {print} END{if(!d) print k"=\""v"\""}' "${CONF}" > "${tmp}" \
    && mv -f "${tmp}" "${CONF}" && sync
}
config_hash() { load_config; for k in ${HASH_KEYS}; do eval "printf '%s=%s;' ${k} \"\${${k}}\""; done | sha256sum | cut -c1-16; }

is_module_disabled() { [ -f "${MODDIR}/disable" ] || [ -f "${MODDIR}/remove" ]; }
is_pid_running()     { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
wait_boot_completed(){ until [ "$(getprop sys.boot_completed)" = 1 ]; do sleep 2; done; }
ce_ready()           { [ "$(getprop sys.user.0.ce_available)" = true ]; }
wakelock_on()        { echo x-route > /sys/power/wake_lock 2>/dev/null; }
wakelock_off()       { echo x-route > /sys/power/wake_unlock 2>/dev/null; }

# CIDR 工具（08 使用）
ip2int() { o="$1"; a="${o%%.*}"; o="${o#*.}"; b="${o%%.*}"; o="${o#*.}"; c="${o%%.*}"; d="${o#*.}"
  echo $(( (a<<24)|(b<<16)|(c<<8)|d )); }
valid_cidr()  { echo "$1" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}/([89]|[12][0-9]|3[0-2])$'; }
cidr_private(){ # 10/8 172.16/12 192.168/16 100.64/10
  n="$(ip2int "${1%/*}")"
  [ $((n>>24)) -eq 10 ] || [ $((n>>20)) -eq $(( (172<<4)|1 )) ] || [ $((n>>16)) -eq $(( (192<<8)|168 )) ] || [ $((n>>22)) -eq $(( (100<<2)|1 )) ]; }
cidr_overlap(){ # $1 $2 两个 CIDR 是否重叠
  a="$(ip2int "${1%/*}")"; pa="${1#*/}"; b="$(ip2int "${2%/*}")"; pb="${2#*/}"
  p=$(( pa < pb ? pa : pb )); m=$(( p==0 ? 0 : (0xFFFFFFFF << (32-p)) & 0xFFFFFFFF ))
  [ $(( a & m )) -eq $(( b & m )) ]; }
```

`valid_kv` 按约束表写成 `case "$1" in XR_EXPOSE) case "$2" in off|hotspot|all) ;; *) return 1;; esac;; ...`。未知的键一律返回 1。

## `module/customize.sh`（只做安装）

```sh
SKIPUNZIP=0
XR_DATA=/data/adb/x-route
ui_print "- x-route v2"
avail_mb="$(df -m /data | awk 'NR==2{print $4}')"
[ "${avail_mb}" -ge 1536 ] || abort "! /data 剩余空间不足 1.5GB"
mkdir -p "${XR_DATA}/config" "${XR_DATA}/images" "${XR_DATA}/run" "${XR_DATA}/log"
[ -f "${XR_DATA}/config/config.conf" ] || cp "${MODPATH}/config/config.conf" "${XR_DATA}/config/config.conf"
cp -f "${MODPATH}/data/Image" "${XR_DATA}/images/Image"
if [ ! -f "${XR_DATA}/images/openwrt-rootfs.raw" ]; then
  ui_print "- 解压系统盘"
  gzip -dc "${MODPATH}/data/openwrt-rootfs.raw.gz" > "${XR_DATA}/images/openwrt-rootfs.raw" || abort "! 解压失败"
  . "${XR_DATA}/config/config.conf"
  truncate -s "${VM_DISK_SIZE_MB}M" "${XR_DATA}/images/openwrt-rootfs.raw"
fi
cp -f "${MODPATH}/data/openwrt-rootfs.raw.gz" "${XR_DATA}/images/openwrt-rootfs.raw.gz"   # 留给 xr reset-disk 使用
rm -rf "${MODPATH}/data"
set_perm_recursive "${MODPATH}" 0 0 0755 0644
for f in xr vm.sh net.sh common.sh; do set_perm "${MODPATH}/scripts/${f}" 0 0 0755; done
chmod 0700 "${XR_DATA}"
ui_print "- 安装完成。重启后在 WebUI 中手动启动。VM 登录：root，上游默认空密码"
```

已有 `config.conf` 和 `openwrt-rootfs.raw` 时保留它们，这是用户数据。重新刷入时不做任何版本判断或迁移。

## `module/service.sh`

```sh
#!/system/bin/sh
MODDIR="${0%/*}"; . "${MODDIR}/scripts/common.sh"
ensure_dirs
wait_boot_completed
is_module_disabled && exit 0
rm -f "${RUN}"/*.pid "${RUN}/crosvm.sock" "${RUN}/routes.active"
if [ -f "${FUSE}" ]; then
  log_err service "上次启动 VM 后整机复位，熔断"
  date '+%F %T' > "${TRIPPED}"; rm -f "${FUSE}"; set_kv AUTOSTART 0; sync
  exit 0
fi
load_config
[ "${AUTOSTART}" = 1 ] || exit 0
[ -f "${TRIPPED}" ] && exit 0
[ "$(cat "${LAST_GOOD}" 2>/dev/null)" = "$(config_hash)" ] || { log_info service "last_good 与当前配置不一致，跳过自启"; exit 0; }
sleep "${AUTOSTART_DELAY_SEC}"
until ce_ready; do sleep 5; done
sh "${MODDIR}/scripts/xr" start
```

## `module/action.sh`

```sh
#!/system/bin/sh
MODDIR="${0%/*}"
if sh "${MODDIR}/scripts/xr" running; then sh "${MODDIR}/scripts/xr" stop; else sh "${MODDIR}/scripts/xr" start; fi
sh "${MODDIR}/scripts/xr" status-line
```

## `module/uninstall.sh`

```sh
#!/system/bin/sh
MODDIR="${0%/*}"
sh "${MODDIR}/scripts/xr" stop 2>/dev/null
sh "${MODDIR}/scripts/net.sh" down 2>/dev/null
echo x-route > /sys/power/wake_unlock 2>/dev/null
rm -rf /data/adb/x-route
```

## `scripts/check-config-keys.sh`（供 CI 使用）

检查 `module/config/config.conf` 里的键集合、`common.sh` 的 `KEYS` 和 `valid_kv` 中 case 分支覆盖的键，三者必须完全一致。

```sh
#!/bin/sh
set -e
a="$(sed -n 's/^\([A-Z_]*\)=.*/\1/p' module/config/config.conf | sort)"
b="$(sed -n 's/^KEYS="\(.*\)"/\1/p' module/scripts/common.sh | tr ' ' '\n' | sort)"
c="$(sed -n '/^valid_kv()/,/^}/p' module/scripts/common.sh | grep -oE '^ *[A-Z_|]+\)' | tr -d ' )' | tr '|' '\n' | sort -u)"
[ "$a" = "$b" ] && [ "$a" = "$c" ] || { echo "config keys mismatch"; exit 1; }
```

## 验收

- CI 的 shellcheck、`sh -n` 和 `check-config-keys.sh` 全部通过。
- 刷入后，`/data/adb/x-route/{config,images,run,log}` 都存在，`openwrt-rootfs.raw` 的大小等于 `VM_DISK_SIZE_MB`。
- 重启后 VM 没有启动，`xr.log` 中没有错误。
- 卸载后重启，`/data/adb/x-route` 不存在。
