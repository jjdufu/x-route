#!/system/bin/sh
# shellcheck disable=SC2034  # 本文件定义的变量供引入它的脚本使用
# ==============================================================================
# XRouter: 公共定义与工具函数（被其他脚本 . 引入）
# 兼容 Android mksh（32 位算术）与 busybox ash。
# ==============================================================================
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
XR_DATA="${XR_DATA:-/data/adb/xrouter}"
BB="${BB:-/data/adb/ksu/bin/busybox}"
MEDIA="${XR_MEDIA:-/data/media/0}"   # 共享目录允许的根（底层路径，不用 FUSE 的 /storage/emulated/0）

# /sdcard、/storage/emulated/0 与 MEDIA（默认 /data/media/0）视为同一处存储
canon_share_path() {
  _p="$1"
  case "$_p" in
    /sdcard) _p="${MEDIA}" ;;
    /sdcard/*) _p="${MEDIA}/${_p#/sdcard/}" ;;
    /storage/emulated/0) _p="${MEDIA}" ;;
    /storage/emulated/0/*) _p="${MEDIA}/${_p#/storage/emulated/0/}" ;;
  esac
  echo "$_p"
}

rotate_file() {
  _f="$1"; [ -f "$_f" ] || return 0
  _sz="$(stat -c %s "$_f" 2>/dev/null || echo 0)"
  [ "$_sz" -gt 524288 ] || return 0
  if [ "$2" = mv ]; then
    mv -f "$_f" "${_f}.1"
  else
    cp -f "$_f" "${_f}.1" 2>/dev/null
    : > "$_f"
  fi
}

rotate_all_logs() {
  rotate_file "${LOG}" mv
  rotate_debug
  rotate_file "${XR_DATA}/log/vmm.log"
  rotate_file "${RUN}/console.log"
  rotate_file "${RUN}/vm-ctl.log"
}

# xr0 的 IPv6 ULA 网段（固定，不随 XR_NET_PREFIX 变化）：手机 ::1，虚拟机 ::254
XR_NET6="fd7a:7872:e9"
CONF="${XR_DATA}/config/config.conf"
RUN="${XR_DATA}/run"
LOG="${XR_DATA}/log/module.log"
# 开机过程日志（WebUI 瀑布）：每次启动清空，稳定或停止后删除；只在文件存在时追加，平时零开销
BOOT_LOG="${RUN}/boot.log"
IMG="${XR_DATA}/images"
ROOTFS="${IMG}/openwrt-rootfs.img"
FUSE="${XR_DATA}/config/vm_booting.fuse"
TRIPPED="${XR_DATA}/config/tripped"
LAST_GOOD="${XR_DATA}/config/last_good"
KEYS="XR_NET_PREFIX XR_VM_MAC XR_DNS XR_EXPOSE XR_MODE VM_CPUS VM_MEM_MB VM_SWIOTLB_MB VM_NET VM_DISK_RW VM_DISK_SIZE_MB VM_WAKELOCK VM_SHARE VM_SHARE_MODE VM_SHARE_PATH AUTOSTART AUTOSTART_DELAY_SEC READY_TIMEOUT_SEC STABLE_SEC XR_NOTIFY_TRAFFIC XR_DEBUG XR_DEBUG_HOURS XR_HOTSPOT_BRIDGE XR_HOTSPOT_SUBNET"
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
  rotate_file "${LOG}" mv
  echo "$(date '+%F %T') [${lv}] [${m}] $*" >> "${LOG}"
  [ -f "${BOOT_LOG}" ] && echo "$(date +%T) [${m}] $*" >> "${BOOT_LOG}"
  return 0
}
# 只写开机过程日志（不进 module.log）：细粒度的启动进度
blog() { # $1=模块 $2...=消息
  [ -f "${BOOT_LOG}" ] || return 0
  _bm="$1"; shift
  echo "$(date +%T) [${_bm}] $*" >> "${BOOT_LOG}"
}
log_info() { log_msg INFO "$@"; }
log_err()  { log_msg ERROR "$@"; }

# ---- 调试模式（XR_DEBUG=1）：超详细日志单独写 log/debug.log，2MB 轮转一次（上限约 4MB）----
# 开关随配置即时生效：各常驻循环每轮重新读取配置，不需要重启虚拟机。
# 到期时间（XR_DEBUG_HOURS，0 表示不自动关闭）记在 config/debug.until，由守护进程检查并自动关闭。
DLOG="${XR_DATA}/log/debug.log"
DEBUG_UNTIL="${XR_DATA}/config/debug.until"
SYSBIN="${XR_SYSBIN:-/system/bin}"
dbg_on() { [ "${XR_DEBUG:-0}" = 1 ]; }
rotate_debug() {
  [ -f "${DLOG}" ] || return 0
  _dsz="$(stat -c %s "${DLOG}" 2>/dev/null || echo 0)"
  [ "$_dsz" -gt 2097152 ] && mv -f "${DLOG}" "${DLOG}.1"
  return 0
}
dlog() { # $1=模块 $2...=消息；仅调试模式写入
  dbg_on || return 0
  _dm="$1"; shift
  mkdir -p "${XR_DATA}/log"
  echo "$(date '+%F %T') [$$] [${_dm}] $*" >> "${DLOG}"
}
# 是否是会改动系统状态的命令（只读查询成功时不记，失败一律记）
_dbg_mut() {
  for _da in "$@"; do
    case "$_da" in
      add|del|delete|replace|flush|set|change|append|prepend|-A|-I|-D|-F|-N|-X|-R|-P|-Z|enable|disable|remove|put) return 0 ;;
    esac
  done
  return 1
}
# 执行系统网络命令：调试模式记下命令、返回值和错误输出（错误输出照常交给调用方）
xr_exec() {
  # 开机中：改动系统状态的网络命令也逐条进开机日志（纯 shell 拼接，不额外起进程）
  if [ -f "${BOOT_LOG}" ] && _dbg_mut "$@"; then
    _bc="$*"; printf '%s\n' "  \$ ${_bc#"${SYSBIN}/"}" >> "${BOOT_LOG}"
  fi
  dbg_on || { "$@"; return $?; }
  _def="$(mktemp "${RUN}/.dbg.XXXXXX" 2>/dev/null || echo "${RUN}/.dbg.$$")"
  "$@" 2>"$_def"; _drc=$?
  _de="$(head -c 600 "$_def" 2>/dev/null | tr '\n' ' ')"; rm -f "$_def"
  # rc=141 是 SIGPIPE：读取方（grep -q、head 等）拿够数据先关了管道，不是命令出错，只读命令不记
  if [ "$_drc" = 141 ] && [ -z "$_de" ] && ! _dbg_mut "$@"; then
    :
  elif [ "$_drc" != 0 ] || [ -n "$_de" ] || _dbg_mut "$@"; then
    dlog cmd "rc=${_drc} $(echo "$*" | sed "s#${SYSBIN}/##")${_de:+ | stderr: ${_de}}"
  fi
  [ -n "$_de" ] && printf '%s\n' "$_de" >&2
  return "$_drc"
}
# 到期自动关闭（守护进程定期调用）
debug_expire() {
  dbg_on || return 0
  _du="$(cat "${DEBUG_UNTIL}" 2>/dev/null)"
  case "$_du" in ''|*[!0-9]*|0) return 0 ;; esac
  [ "$(date +%s)" -ge "$_du" ] || return 0
  set_kv XR_DEBUG 0 && rm -f "${DEBUG_UNTIL}"
  XR_DEBUG=0
  log_info debug "调试模式已到期，自动关闭"
}
# 按当前设置重算到期时间
debug_arm() {
  load_config
  if [ "${XR_DEBUG:-0}" = 1 ] && [ "${XR_DEBUG_HOURS:-0}" != 0 ]; then
    echo $(( $(date +%s) + XR_DEBUG_HOURS * 3600 )) > "${DEBUG_UNTIL}"
  else
    rm -f "${DEBUG_UNTIL}"
  fi
}

is_uint()  { case "$1" in ''|*[!0-9]*) return 1;; esac; [ "${#1}" -le 6 ]; }
in_range() { is_uint "$1" && [ "$1" -ge "$2" ] && [ "$1" -le "$3" ]; }
valid_ip4() {
  echo "$1" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}$' || return 1
  o="$1"; for _ in 1 2 3 4; do v="${o%%.*}"; [ "$v" -le 255 ] || return 1; o="${o#*.}"; done
}

# IPv4 点分掩码 -> 前缀长度；非连续掩码返回失败
mask2len() {
  valid_ip4 "$1" || return 1
  _m=$(( $(ip2int "$1") & 0xFFFFFFFF ))
  _p=0
  while [ "$_p" -le 32 ]; do
    [ $(( $(prefix_mask "$_p") & 0xFFFFFFFF )) -eq "$_m" ] && { echo "$_p"; return 0; }
    _p=$((_p+1))
  done
  return 1
}

# 把 IPv6 地址展开为 32 位小写十六进制（不含冒号）。不合法则返回 1。
ip6_expand() {
  printf '%s' "$1" | awk '
  function collect(s, arr,    n, t, i) {
    if (s == "") return 0
    n = split(s, t, /:/)
    for (i = 1; i <= n; i++) {
      if (t[i] == "" || length(t[i]) > 4 || t[i] !~ /^[0-9a-f]+$/) return -1
      arr[i] = t[i]
      while (length(arr[i]) < 4) arr[i] = "0" arr[i]
    }
    return n
  }
  {
    s = tolower($0)
    if (s == "" || s ~ /[^0-9a-f:]/) exit 1
    n = split(s, parts, /::/)
    if (n > 2) exit 1
    if (n == 1) {
      if (s ~ /^:/ || s ~ /:$/) exit 1
      ng = collect(s, g)
      if (ng != 8) exit 1
    } else {
      lg = collect(parts[1], left)
      rg = collect(parts[2], right)
      if (lg < 0 || rg < 0) exit 1
      miss = 8 - lg - rg
      if (miss < 1) exit 1
      ng = 0
      for (i = 1; i <= lg; i++) g[++ng] = left[i]
      for (i = 1; i <= miss; i++) g[++ng] = "0000"
      for (i = 1; i <= rg; i++) g[++ng] = right[i]
    }
    out = ""
    for (i = 1; i <= 8; i++) out = out g[i]
    print out
  }'
}

# $1=32 位十六进制 $2=前缀长度 -> 主机位置零后的 32 位十六进制
ip6_mask() {
  _h="$1"; _p="$2"
  [ "${#_h}" -eq 32 ] || return 1
  is_uint "$_p" && [ "$_p" -ge 0 ] && [ "$_p" -le 128 ] || return 1
  _fn=$((_p / 4)); _rem=$((_p % 4))
  if [ "$_fn" -gt 0 ]; then _out="$(printf '%s' "$_h" | cut -c1-"$_fn")"; else _out=""; fi
  if [ "$_fn" -lt 32 ]; then
    if [ "$_rem" -gt 0 ]; then
      _nib="$(printf '%s' "$_h" | cut -c$((_fn + 1)))"
      case "$_nib" in
        a|A) _val=10 ;; b|B) _val=11 ;; c|C) _val=12 ;; d|D) _val=13 ;; e|E) _val=14 ;; f|F) _val=15 ;;
        [0-9]) _val="$_nib" ;; *) return 1 ;;
      esac
      case "$_rem" in 1) _mod=8 ;; 2) _mod=4 ;; 3) _mod=2 ;; esac
      _val=$((_val - (_val % _mod)))
      _out="${_out}$(printf '%x' "$_val")"
      _z=$((32 - _fn - 1))
    else
      _z=$((32 - _fn))
    fi
    while [ "$_z" -gt 0 ]; do _out="${_out}0"; _z=$((_z - 1)); done
  fi
  echo "$_out"
}

# 把 OpenVPN 上报的路由规范成 CIDR：裸 IPv4 -> /32，裸 IPv6 -> /128，点分掩码换成前缀长度
norm_route() {
  _t="$1"
  case "$_t" in ""|default) return 1 ;; esac
  case "$_t" in
    *.*.*.*/*.*.*.*)
      _a="${_t%/*}"; _m="${_t#*/}"
      valid_ip4 "$_a" || return 1
      _p="$(mask2len "$_m")" || return 1
      [ "$_p" -ge 1 ] && [ "$_p" -le 32 ] || return 1
      echo "$_a/$_p"; return 0
      ;;
  esac
  if valid_ip4 "$_t"; then echo "$_t/32"; return 0; fi
  case "$_t" in
    *.*.*.*/*)
      echo "$_t" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}/([1-9]|[12][0-9]|3[0-2])$' && valid_ip4 "${_t%/*}" && { echo "$_t"; return 0; }
      return 1
      ;;
    *:*)
      case "$_t" in
        */*)
          _a="${_t%/*}"; _p="${_t#*/}"
          [ "$_a" != "$_t" ] && is_uint "$_p" && [ "$_p" -ge 1 ] && [ "$_p" -le 128 ] || return 1
          ip6_expand "$_a" >/dev/null || return 1
          echo "$_t"; return 0
          ;;
        *)
          ip6_expand "$_t" >/dev/null || return 1
          echo "$_t/128"; return 0
          ;;
      esac
      ;;
  esac
  return 1
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
      case "$2" in local|lan|all) ;; *) return 1;; esac ;;
    XR_MODE)
      case "$2" in global|bypass) ;; *) return 1;; esac ;;
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
      _sp="$(canon_share_path "$2")"
      case "$_sp" in "${MEDIA}"|"${MEDIA}"/*) ;; *) return 1;; esac
      case "$_sp" in *..*|*\"*|*\'*|*' '*|*:*|*,*|*'$'*|*'`'*|*\\*) return 1;; esac
      [ -d "$_sp" ] ;;
    AUTOSTART_DELAY_SEC)
      in_range "$2" 0 600 ;;
    READY_TIMEOUT_SEC|STABLE_SEC)
      in_range "$2" 60 900 ;;
    XR_NOTIFY_TRAFFIC)
      case "$2" in total|wan|lan) ;; *) return 1;; esac ;;
    XR_DEBUG|XR_HOTSPOT_BRIDGE)
      case "$2" in 0|1) ;; *) return 1;; esac ;;
    XR_DEBUG_HOURS)
      case "$2" in 0|1|6|24|72) ;; *) return 1;; esac ;;
    XR_HOTSPOT_SUBNET)   # 空 = 跟随安卓热点网段；否则「网关/前缀」，如 10.233.0.1/24
      [ -z "$2" ] || { hs_subnet_valid "$2" && ! hs_subnet_clash "$2" >/dev/null; } ;;
    *) return 1 ;;
  esac
}

set_kv() { # $1=键 $2=值：先校验，写临时文件，mv 替换，再 sync
  _sv="$2"
  [ "$1" = VM_SHARE_PATH ] && _sv="$(canon_share_path "$2")"
  valid_kv "$1" "$_sv" || return 1
  tmp="${CONF}.tmp.$$"
  awk -v k="$1" -v v="$_sv" 'BEGIN{d=0} index($0, k"=")==1 {print k"=\""v"\""; d=1; next} {print} END{if(!d) print k"=\""v"\""}' "${CONF}" > "${tmp}" \
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
wakelock_on()        { { echo xrouter > /sys/power/wake_lock; } 2>/dev/null; }
wakelock_off()       { { echo xrouter > /sys/power/wake_unlock; } 2>/dev/null; }

# ---- CIDR 工具（08 使用）。mksh 只有 32 位有符号算术：只用按位与和相等比较，不做有符号的移位比较 ----
# 注意：sh 没有局部变量，以下函数只用 _ 前缀变量，避免覆盖调用方的 n/b/c 等循环变量
ip2int() { _o="$1"; _a="${_o%%.*}"; _o="${_o#*.}"; _b="${_o%%.*}"; _o="${_o#*.}"; _c="${_o%%.*}"; _d="${_o#*.}"
  echo $(( (_a<<24) | (_b<<16) | (_c<<8) | _d )); }
prefix_mask() { [ "$1" -eq 0 ] && { echo 0; return; }; echo $(( (0xFFFFFFFF << (32-$1)) & 0xFFFFFFFF )); }
valid_cidr() {
  # 接受规范 CIDR，以及裸主机地址、IPv4 点分掩码（先经 norm_route）
  _c="$(norm_route "$1")" || return 1
  case "$_c" in
    *:*)
      _p="${_c#*/}"
      is_uint "$_p" && [ "$_p" -ge 1 ] && [ "$_p" -le 128 ]
      ;;
    *)
      echo "$_c" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}/([1-9]|[12][0-9]|3[0-2])$' && valid_ip4 "${_c%/*}"
      ;;
  esac
}
cidr_private() { # 10/8 172.16/12 192.168/16 100.64/10（按网络地址判断，前缀不得比所在私网段更宽）
  _o="${1%/*}"; _p="${1#*/}"; _a="${_o%%.*}"; _o="${_o#*.}"; _b="${_o%%.*}"
  if [ "$_a" -eq 10 ]; then [ "$_p" -ge 8 ]
  elif [ "$_a" -eq 172 ] && [ "$_b" -ge 16 ] && [ "$_b" -le 31 ]; then [ "$_p" -ge 12 ]
  elif [ "$_a" -eq 192 ] && [ "$_b" -eq 168 ]; then [ "$_p" -ge 16 ]
  elif [ "$_a" -eq 100 ] && [ "$_b" -ge 64 ] && [ "$_b" -le 127 ]; then [ "$_p" -ge 10 ]
  else return 1; fi
}
cidr_aligned() {
  _c="$(norm_route "$1")" || return 1
  case "$_c" in
    *:*)
      _h="$(ip6_expand "${_c%/*}")" || return 1
      _z="$(ip6_mask "$_h" "${_c#*/}")" || return 1
      [ "$_h" = "$_z" ]
      ;;
    *)
      _n="$(ip2int "${_c%/*}")"; _m="$(prefix_mask "${_c#*/}")"
      [ $(( _n & _m )) -eq $(( _n & 0xFFFFFFFF )) ]
      ;;
  esac
}
cidr_overlap() { # $1 $2 两个 CIDR 是否重叠（不同协议视为不重叠）
  _ca="$(norm_route "$1")" || return 1
  _cb="$(norm_route "$2")" || return 1
  case "$_ca" in *:*) _va=6 ;; *) _va=4 ;; esac
  case "$_cb" in *:*) _vb=6 ;; *) _vb=4 ;; esac
  [ "$_va" = "$_vb" ] || return 1
  if [ "$_va" = 4 ]; then
    _x="$(ip2int "${_ca%/*}")"; _pa="${_ca#*/}"; _y="$(ip2int "${_cb%/*}")"; _pb="${_cb#*/}"
    if [ "$_pa" -lt "$_pb" ]; then _p="$_pa"; else _p="$_pb"; fi
    _m="$(prefix_mask "$_p")"
    [ $(( _x & _m )) -eq $(( _y & _m )) ]
  else
    _x="$(ip6_expand "${_ca%/*}")" || return 1
    _y="$(ip6_expand "${_cb%/*}")" || return 1
    _pa="${_ca#*/}"; _pb="${_cb#*/}"
    if [ "$_pa" -lt "$_pb" ]; then _p="$_pa"; else _p="$_pb"; fi
    [ "$(ip6_mask "$_x" "$_p")" = "$(ip6_mask "$_y" "$_p")" ]
  fi
}
cidr_net() { # 网关 CIDR -> 网段（主机位清零），如 10.233.0.1/24 -> 10.233.0.0/24
  _x="$(ip2int "${1%/*}")"; _m="$(prefix_mask "${1#*/}")"; _x=$(( _x & _m ))
  echo "$(( (_x >> 24) & 255 )).$(( (_x >> 16) & 255 )).$(( (_x >> 8) & 255 )).$(( _x & 255 ))/${1#*/}"
}

# 固定热点网段格式：私网 IPv4 网关地址/16~30，网关不能是网络地址或广播地址（不含 100.64/10 运营商地址段）
hs_subnet_valid() {
  echo "$1" | grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}/(1[6-9]|2[0-9]|30)$' && valid_ip4 "${1%/*}" || return 1
  [ "${1%%.*}" != 100 ] && cidr_private "$1" || return 1
  _x="$(ip2int "${1%/*}")"; _m="$(prefix_mask "${1#*/}")"
  _h=$(( _x & ~_m & 0xFFFFFFFF )); _b=$(( ~_m & 0xFFFFFFFF ))
  [ "$_h" -ne 0 ] && [ "$_h" -ne "$_b" ]
}

# 固定热点网段是否与 xr0 链路网段、手机现有网卡地址（上游 Wi-Fi / 蜂窝 / VPN 等）冲突
# $1=网关 CIDR $2=要跳过的网卡（当前热点网卡）。冲突时输出冲突对象并返回 0
hs_subnet_clash() {
  _xn="${XR_NET_PREFIX:-172.29.233}.0/24"
  cidr_overlap "$1" "$_xn" && { echo "xr0 ${_xn}"; return 0; }
  _sb="${XR_SYSBIN:-/system/bin}"
  # 热点 / USB 共享等下游网卡（local_network 表）的地址由安卓随机分配、桥接后不再使用，不算冲突
  _sk=" lo xr0 xr1 xrbr1 ${2:-} $("${_sb}/ip" route show table local_network 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="dev") printf "%s ", $(i+1)}') "
  for _l in $("${_sb}/ip" -4 -o addr show 2>/dev/null | awk '{print $2 "=" $4}'); do
    _d="${_l%%=*}"; _d="${_d%%@*}"; _a="${_l#*=}"
    case "$_sk" in *" $_d "*) continue ;; esac
    cidr_overlap "$1" "$_a" && { echo "${_d} ${_a}"; return 0; }
  done
  return 1
}
