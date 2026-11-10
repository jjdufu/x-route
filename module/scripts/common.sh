#!/system/bin/sh
# shellcheck disable=SC2034  # 本文件定义的变量供引入它的脚本使用
# ==============================================================================
# x-route v2: 公共定义与工具函数（被其他脚本 . 引入）
# 兼容 Android mksh（32 位算术）与 busybox ash。
# ==============================================================================
MODDIR="${MODDIR:-/data/adb/modules/x_route}"
XR_DATA="${XR_DATA:-/data/adb/x-route}"
BB="${BB:-/data/adb/ksu/bin/busybox}"
CONF="${XR_DATA}/config/config.conf"
RUN="${XR_DATA}/run"
LOG="${XR_DATA}/log/xr.log"
IMG="${XR_DATA}/images"
ROOTFS="${IMG}/openwrt-rootfs.img"
FUSE="${XR_DATA}/config/vm_booting.fuse"
TRIPPED="${XR_DATA}/config/tripped"
LAST_GOOD="${XR_DATA}/config/last_good"
KEYS="XR_NET_PREFIX XR_VM_MAC XR_DNS XR_EXPOSE VM_CPUS VM_MEM_MB VM_SWIOTLB_MB VM_NET VM_DISK_RW VM_DISK_SIZE_MB VM_WAKELOCK VM_SHARE VM_SHARE_MODE VM_SHARE_PATH VPN_MODE VPN_ROUTES AUTOSTART AUTOSTART_DELAY_SEC READY_TIMEOUT_SEC STABLE_SEC"
# 影响 VM 稳定性的键：只要这些键变化，last_good 就失效
HASH_KEYS="XR_NET_PREFIX VM_CPUS VM_MEM_MB VM_SWIOTLB_MB VM_NET VM_DISK_RW VM_SHARE VM_SHARE_MODE"
# rw 共享的属主映射参数（crosvm --shared-dir 选项）。需先在真机上确认 crosvm 支持的写法（见 docs/TESTING.md 第 0 节）；
# 为空表示不支持 rw，此时 VM_SHARE_MODE=rw 被拒绝。
RW_MAP_OPT="${RW_MAP_OPT:-}"

ensure_dirs()  { mkdir -p "${XR_DATA}/config" "${RUN}" "${XR_DATA}/log" "${IMG}"; }
load_config()  { [ -f "${CONF}" ] && . "${CONF}"; }

log_msg() { # $1=级别 $2=模块 $3...=消息
  lv="$1"; m="$2"; shift 2
  mkdir -p "${XR_DATA}/log"
  if [ -f "${LOG}" ] && [ "$(stat -c %s "${LOG}" 2>/dev/null || echo 0)" -gt 524288 ]; then mv -f "${LOG}" "${LOG}.1"; fi
  echo "$(date '+%F %T') [${lv}] [${m}] $*" >> "${LOG}"
}
log_info() { log_msg INFO "$@"; }
log_err()  { log_msg ERROR "$@"; }

is_uint()  { case "$1" in ''|*[!0-9]*) return 1;; esac; [ "${#1}" -le 6 ]; }
in_range() { is_uint "$1" && [ "$1" -ge "$2" ] && [ "$1" -le "$3" ]; }
valid_ip4() {
  echo "$1" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}$' || return 1
  o="$1"; for _ in 1 2 3 4; do v="${o%%.*}"; [ "$v" -le 255 ] || return 1; o="${o#*.}"; done
}

valid_kv() { # $1=键 $2=值，按 docs/v2-plan/05 的约束表校验
  case "$1" in
    XR_NET_PREFIX)
      echo "$2" | grep -qE '^(10\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}$' && valid_ip4 "$2.0" ;;
    XR_VM_MAC)
      echo "$2" | grep -qE '^52:54:00(:[0-9a-f]{2}){3}$' ;;
    XR_DNS)
      n=0; for d in $2; do valid_ip4 "$d" || return 1; n=$((n+1)); done; [ "$n" -ge 1 ] && [ "$n" -le 3 ] ;;
    XR_EXPOSE)
      case "$2" in off|hotspot|all) ;; *) return 1;; esac ;;
    VM_CPUS)
      in_range "$2" 1 4 ;;
    VM_MEM_MB)
      in_range "$2" 384 2048 ;;
    VM_SWIOTLB_MB)
      case "$2" in 32|64|128) ;; *) return 1;; esac ;;
    VM_NET|VM_DISK_RW|VM_WAKELOCK|VM_SHARE|AUTOSTART)
      case "$2" in 0|1) ;; *) return 1;; esac ;;
    VM_DISK_SIZE_MB)
      in_range "$2" 512 16384 ;;
    VM_SHARE_MODE)
      case "$2" in ro) ;; rw) [ -n "${RW_MAP_OPT}" ] ;; *) return 1;; esac ;;
    VM_SHARE_PATH)
      case "$2" in /data/media/0|/data/media/0/*) ;; *) return 1;; esac
      case "$2" in *..*|*\"*|*\'*|*' '*) return 1;; esac
      [ -d "$2" ] ;;
    VPN_MODE)
      case "$2" in terminal|site) ;; *) return 1;; esac ;;
    VPN_ROUTES)
      [ -z "$2" ] && return 0
      echo "$2" | grep -qE '^[0-9./,]+$' || return 1
      _k=0; for _r in $(echo "$2" | tr ',' ' '); do
        valid_cidr "$_r" && cidr_private "$_r" && cidr_aligned "$_r" || return 1; _k=$((_k+1)); done
      [ "$_k" -le 64 ] ;;
    AUTOSTART_DELAY_SEC)
      in_range "$2" 0 600 ;;
    READY_TIMEOUT_SEC|STABLE_SEC)
      in_range "$2" 60 900 ;;
    *) return 1 ;;
  esac
}

set_kv() { # $1=键 $2=值：先校验，写临时文件，mv 替换，再 sync
  valid_kv "$1" "$2" || return 1
  tmp="${CONF}.tmp.$$"
  awk -v k="$1" -v v="$2" 'BEGIN{d=0} index($0, k"=")==1 {print k"=\""v"\""; d=1; next} {print} END{if(!d) print k"=\""v"\""}' "${CONF}" > "${tmp}" \
    && mv -f "${tmp}" "${CONF}" && sync
}

config_hash() {
  load_config
  for k in ${HASH_KEYS}; do eval "printf '%s=%s;' ${k} \"\${${k}}\""; done | sha256sum | cut -c1-16
}

is_module_disabled() { [ -f "${MODDIR}/disable" ] || [ -f "${MODDIR}/remove" ]; }
is_pid_running()     { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
wait_boot_completed(){ until [ "$(getprop sys.boot_completed)" = 1 ]; do sleep 2; done; }
ce_ready()           { [ "$(getprop sys.user.0.ce_available)" = true ]; }
wakelock_on()        { { echo x-route > /sys/power/wake_lock; } 2>/dev/null; }
wakelock_off()       { { echo x-route > /sys/power/wake_unlock; } 2>/dev/null; }

# ---- CIDR 工具（08 使用）。mksh 只有 32 位有符号算术：只用按位与和相等比较，不做有符号的移位比较 ----
# 注意：sh 没有局部变量，以下函数只用 _ 前缀变量，避免覆盖调用方的 n/b/c 等循环变量
ip2int() { _o="$1"; _a="${_o%%.*}"; _o="${_o#*.}"; _b="${_o%%.*}"; _o="${_o#*.}"; _c="${_o%%.*}"; _d="${_o#*.}"
  echo $(( (_a<<24) | (_b<<16) | (_c<<8) | _d )); }
prefix_mask() { [ "$1" -eq 0 ] && { echo 0; return; }; echo $(( (0xFFFFFFFF << (32-$1)) & 0xFFFFFFFF )); }
valid_cidr() {
  echo "$1" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}/([89]|[12][0-9]|3[0-2])$' && valid_ip4 "${1%/*}"
}
cidr_private() { # 10/8 172.16/12 192.168/16 100.64/10（按网络地址判断，前缀不得比所在私网段更宽）
  _o="${1%/*}"; _p="${1#*/}"; _a="${_o%%.*}"; _o="${_o#*.}"; _b="${_o%%.*}"
  if [ "$_a" -eq 10 ]; then [ "$_p" -ge 8 ]
  elif [ "$_a" -eq 172 ] && [ "$_b" -ge 16 ] && [ "$_b" -le 31 ]; then [ "$_p" -ge 12 ]
  elif [ "$_a" -eq 192 ] && [ "$_b" -eq 168 ]; then [ "$_p" -ge 16 ]
  elif [ "$_a" -eq 100 ] && [ "$_b" -ge 64 ] && [ "$_b" -le 127 ]; then [ "$_p" -ge 10 ]
  else return 1; fi
}
cidr_aligned() { _n="$(ip2int "${1%/*}")"; _m="$(prefix_mask "${1#*/}")"; [ $(( _n & _m )) -eq $(( _n & 0xFFFFFFFF )) ]; }
cidr_overlap() { # $1 $2 两个 CIDR 是否重叠
  _x="$(ip2int "${1%/*}")"; _pa="${1#*/}"; _y="$(ip2int "${2%/*}")"; _pb="${2#*/}"
  if [ "$_pa" -lt "$_pb" ]; then _p="$_pa"; else _p="$_pb"; fi
  _m="$(prefix_mask "$_p")"
  [ $(( _x & _m )) -eq $(( _y & _m )) ]
}
