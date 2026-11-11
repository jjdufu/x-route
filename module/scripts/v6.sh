#!/system/bin/sh
# XRouter: IPv6 前缀共享与邻居代理（不使用 NAT66 / ip6tables nat）。
# 由 net.sh 引入。部分运营商只对手机本机 IID 做邻居学习，额外地址可能收不到回程。
# shellcheck disable=SC2034
# outs6 / ex 由调用方（sync_all / net_watch）赋值
: "${outs6:=}"
: "${ex:=}"

# MAC -> EUI-64（16 位十六进制，已翻转 U/L 位）
mac_eui64() {
  _mac="$(printf '%s' "$1" | tr 'A-F' 'a-f')"
  echo "$_mac" | grep -qE '^([0-9a-f]{2}:){5}[0-9a-f]{2}$' || return 1
  _o1="${_mac%%:*}"; _r="${_mac#*:}"
  _o2="${_r%%:*}"; _r="${_r#*:}"
  _o3="${_r%%:*}"; _r="${_r#*:}"
  _o4="${_r%%:*}"; _r="${_r#*:}"
  _o5="${_r%%:*}"; _o6="${_r#*:}"
  _d=$((0x$_o1))
  if [ $((_d % 4)) -ge 2 ]; then _d=$((_d - 2)); else _d=$((_d + 2)); fi
  printf '%02x%s%sfffe%s%s\n' "$_d" "$_o2" "$_o3" "$_o4" "$_o5$_o6"
}

# 32 位十六进制 -> 冒号分隔（不压缩）
ip6_format() {
  printf '%s' "$1" | sed 's/..../&:/g; s/:$//'
}

v6_is_ll()  { case "$(ip6_expand "${1%/*}" 2>/dev/null)" in fe8*|fe9*|fea*|feb*) return 0 ;; *) return 1 ;; esac; }
v6_is_ula() { case "$(ip6_expand "${1%/*}" 2>/dev/null)" in fc*|fd*) return 0 ;; *) return 1 ;; esac; }

# $1=前缀CIDR $2=16 位十六进制 IID -> 完整地址（不含前缀长度）
v6_compose() {
  _ph="$(ip6_expand "${1%/*}")" || return 1
  _pn="${1#*/}"
  [ "$_pn" -le 64 ] || return 1
  _net="$(ip6_mask "$_ph" "$_pn")" || return 1
  [ "${#2}" -eq 16 ] || return 1
  ip6_format "$(printf '%s' "$_net" | cut -c1-16)$2"
}

v6_addr_eq() {
  [ "$(ip6_expand "${1%/*}")" = "$(ip6_expand "${2%/*}")" ]
}

# $1=前缀CIDR  $2=已占用地址列表  $3=可选 MAC
# 选出前缀中一个未被占用的地址，写成 addr/prefixlen
v6_pick_addr() {
  _pfx="$1"; _busy="$2"; _mac="${3:-${XR_VM_MAC:-52:54:00:1d:e9:fe}}"
  _len="${_pfx#*/}"
  [ "$_len" -ge 48 ] && [ "$_len" -le 64 ] || return 1
  _cands=""
  _e="$(mac_eui64 "$_mac")" && _cands="$_e"
  _cands="$_cands 787200e900000254"
  _i=1
  while [ "$_i" -le 8 ]; do
    _cands="$_cands 787200e90000$(printf '%04x' "$_i")"
    _i=$((_i + 1))
  done
  for _iid in $_cands; do
    _a="$(v6_compose "$_pfx" "$_iid")" || continue
    _hit=0
    for _b in $_busy; do
      [ -n "$_b" ] || continue
      v6_addr_eq "$_a" "$_b" && { _hit=1; break; }
    done
    [ "$_hit" = 0 ] && { echo "$_a/$_len"; return 0; }
  done
  log_err net "无法在 $_pfx 中分到未占用的 IPv6 地址"
  return 1
}

# 列出网卡上的全局 IPv6 地址（CIDR），跳过链路本地与 ULA
v6_iface_globals() {
  [ -n "$1" ] || return 0
  ip -6 -o addr show dev "$1" 2>/dev/null | awk '{print $4}' | while IFS= read -r _a; do
    [ -n "$_a" ] || continue
    v6_is_ll "$_a" && continue
    v6_is_ula "$_a" && continue
    echo "$_a"
  done
}

# 从上游出口中挑选可共享的前缀（优先 /64，其次 48–63；/128 无法共享）
# 输出：prefix_cidr  iface  busy_addrs（空格分隔，同一行 prefix 与 iface 用文件分开写）
v6_discover() {
  rm -f "${RUN}/net.v6pfx" "${RUN}/net.v6upif"
  _busy=""
  _best=""; _bestlen=128; _bestif=""
  for _i in $outs6 $(tether_ifaces); do
    [ -n "$_i" ] || continue
    [ "$_i" = "${XR_IF}" ] && continue
    for _a in $(v6_iface_globals "$_i"); do
      _busy="$_busy $_a"
      _len="${_a#*/}"
      is_uint "$_len" || continue
      if [ "$_len" -ge 48 ] && [ "$_len" -le 64 ] && [ "$_len" -lt "$_bestlen" ]; then
        _h="$(ip6_expand "${_a%/*}")" || continue
        _net="$(ip6_mask "$_h" "$_len")" || continue
        _best="$(ip6_format "$_net")/$_len"
        _bestlen="$_len"
        _bestif="$_i"
      fi
    done
  done
  # 优先采用第一个上游出口上的前缀（与 5G/WAN 对齐），若其前缀长度相同或更好
  for _i in $outs6; do
    [ -n "$_i" ] || continue
    for _a in $(v6_iface_globals "$_i"); do
      _len="${_a#*/}"
      if ! is_uint "$_len" || [ "$_len" -lt 48 ] || [ "$_len" -gt 64 ]; then continue; fi
      _h="$(ip6_expand "${_a%/*}")" || continue
      _net="$(ip6_mask "$_h" "$_len")" || continue
      _best="$(ip6_format "$_net")/$_len"
      _bestif="$_i"
      break 2
    done
  done
  [ -n "$_best" ] || return 1
  echo "$_best" > "${RUN}/net.v6pfx"
  echo "$_bestif" > "${RUN}/net.v6upif"
  echo "$_busy"
}

v6_tether_offload_off() {
  # 硬件加速共享可能把蜂窝前缀直接交给下游，绕过本机规则。尽量关掉。
  command -v settings >/dev/null 2>&1 && settings put global tether_offload_disabled 1 >/dev/null 2>&1
  ndc tether offload disable >/dev/null 2>&1
  { setprop persist.net.tethering.offload 0; } 2>/dev/null
  return 0
}

v6_sysw_save() { # $1=path $2=value  首次把原值记到 $3
  [ -f "$3" ] || cat "$1" 2>/dev/null > "$3" || echo "" > "$3"
  sysw "$1" "$2"
}

v6_write_wan() {
  mkdir -p "${RUN}/guest/host"
  if [ -n "$1" ]; then
    echo "$1" > "${RUN}/guest/host/ipv6.wan"
  else
    : > "${RUN}/guest/host/ipv6.wan"
  fi
}

v6_proxy_remove() {
  _addr="$(cat "${RUN}/net.v6vm" 2>/dev/null)"
  _if="$(cat "${RUN}/net.v6upif" 2>/dev/null)"
  if [ -n "$_addr" ]; then
    _a="${_addr%/*}"
    [ -n "$_if" ] && ip6 neigh del proxy "$_a" dev "$_if" 2>/dev/null
    for _t in $(tether_ifaces); do
      ip6 neigh del proxy "$_a" dev "$_t" 2>/dev/null
    done
    ip6 route del "$_addr" dev "${XR_IF}" 2>/dev/null
    ip6 route del "$_a/128" dev "${XR_IF}" 2>/dev/null
  fi
  if [ -n "$_if" ]; then
    sysw "${PROC}/sys/net/ipv6/conf/${_if}/proxy_ndp" 0
  fi
  sysw "${PROC}/sys/net/ipv6/conf/${XR_IF}/proxy_ndp" 0
  if [ -f "${RUN}/v6fwd.orig" ]; then
    sysw "${PROC}/sys/net/ipv6/conf/all/forwarding" "$(cat "${RUN}/v6fwd.orig")"
    rm -f "${RUN}/v6fwd.orig"
  fi
  v6_write_wan ""
  rm -f "${RUN}/net.v6proxy" "${RUN}/net.v6vm" "${RUN}/net.v6pfx"
  # 上游网卡名留给下次 discover 覆盖；down 时一并删除
}

# 安装或刷新邻居代理。成功则写入 net.v6proxy。部分运营商可能只放行本机地址。
v6_proxy_install() {
  _busy="$(v6_discover)" || {
    log_info net "上游没有可共享的 IPv6 前缀（可能仅有本机 /128），无法做邻居代理"
    v6_proxy_remove
    return 1
  }
  _pfx="$(cat "${RUN}/net.v6pfx")"
  _if="$(cat "${RUN}/net.v6upif")"
  _wan="$(v6_pick_addr "$_pfx" "$_busy")" || { v6_proxy_remove; return 1; }
  _a="${_wan%/*}"
  # 已有相同代理则只确保规则还在
  if [ -f "${RUN}/net.v6proxy" ] && [ "$(cat "${RUN}/net.v6vm" 2>/dev/null)" = "$_wan" ]; then
    ip6 neigh replace proxy "$_a" dev "$_if" 2>/dev/null
    v6_write_wan "$_wan"
    return 0
  fi
  # 地址变化时先撤旧的
  [ -f "${RUN}/net.v6proxy" ] && v6_proxy_remove
  # 重新发现（remove 清掉了 pfx 文件）
  _busy="$(v6_discover)" || { return 1; }
  _pfx="$(cat "${RUN}/net.v6pfx")"
  _if="$(cat "${RUN}/net.v6upif")"
  _wan="$(v6_pick_addr "$_pfx" "$_busy")" || return 1
  _a="${_wan%/*}"

  mkdir -p "${PROC}/sys/net/ipv6/conf/all" "${PROC}/sys/net/ipv6/conf/${_if}" "${PROC}/sys/net/ipv6/conf/${XR_IF}"
  v6_sysw_save "${PROC}/sys/net/ipv6/conf/all/forwarding" 1 "${RUN}/v6fwd.orig"
  sysw "${PROC}/sys/net/ipv6/conf/${_if}/forwarding" 1
  sysw "${PROC}/sys/net/ipv6/conf/${XR_IF}/forwarding" 1
  # 开启转发后仍要接收 RA，否则手机自己的前缀会掉
  sysw "${PROC}/sys/net/ipv6/conf/${_if}/accept_ra" 2
  sysw "${PROC}/sys/net/ipv6/conf/${_if}/proxy_ndp" 1
  sysw "${PROC}/sys/net/ipv6/conf/${XR_IF}/proxy_ndp" 1
  ip6 neigh replace proxy "$_a" dev "$_if" 2>/dev/null
  ip6 route replace "$_a/128" dev "${XR_IF}" 2>/dev/null
  v6_tether_offload_off
  echo "$_wan" > "${RUN}/net.v6vm"
  v6_write_wan "$_wan"
  : > "${RUN}/net.v6proxy"
  # 部分运营商只学习手机本机 IID，额外地址的 ND 代理可能没有回程
  log_info net "IPv6 邻居代理 ${_wan} dev ${_if}（运营商若只放行本机地址，则可能无法出网）"
  return 0
}

v6_src_fingerprint() {
  for _i in $outs6 $(tether_ifaces); do
    [ -n "$_i" ] || continue
    v6_iface_globals "$_i"
  done | tr '\n' ' '
}

# 按当前开关与上游前缀安装或拆除代理（不重启虚拟机）
v6_proxy_sync() {
  if vm_v6; then
    if v6_proxy_install; then
      :
    else
      v6_write_wan ""
    fi
  else
    v6_proxy_remove
  fi
  v6_src_fingerprint > "${RUN}/net.v6src"
}

# ip6tables filter 不可用时，用 nft 挂一条 inet 转发链（能力检测，不假设 nat 存在）
v6_nft_setup() {
  command -v nft >/dev/null 2>&1 || return 1
  nft list tables >/dev/null 2>&1 || return 1
  nft add table inet xr_v6 >/dev/null 2>&1 || true
  nft add chain inet xr_v6 fwd '{ type filter hook forward priority -10; policy accept; }' >/dev/null 2>&1 || return 1
  : > "${RUN}/net.fwd6nft"
}

v6_nft_teardown() {
  [ -f "${RUN}/net.fwd6nft" ] || return 0
  nft delete table inet xr_v6 >/dev/null 2>&1
  rm -f "${RUN}/net.fwd6nft"
}

v6_nft_sync() {
  [ -f "${RUN}/net.fwd6nft" ] || return 0
  nft flush chain inet xr_v6 fwd >/dev/null 2>&1 || return 0
  nft add rule inet xr_v6 fwd oifname "${XR_IF}" ct state related,established accept
  nft add rule inet xr_v6 fwd iifname "${XR_IF}" ct state related,established accept
  if vm_v6; then
    for o in $outs6; do nft add rule inet xr_v6 fwd iifname "${XR_IF}" oifname "$o" accept; done
  fi
  for i in $ex; do
    nft add rule inet xr_v6 fwd iifname "$i" oifname "${XR_IF}" ip6 daddr "${NET6}" accept
    for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
      case "$c" in *:*) nft add rule inet xr_v6 fwd iifname "$i" oifname "${XR_IF}" ip6 daddr "$c" accept ;; esac
    done
  done
  for o in $outs6; do
    case " $ex " in *" $o "*) ;; *) nft add rule inet xr_v6 fwd iifname "$o" oifname "${XR_IF}" drop ;; esac
  done
  if [ "$1" != off ]; then
    for i in $(tether_ifaces); do
      if [ "$1" = v6 ]; then
        nft add rule inet xr_v6 fwd iifname "$i" oifname "${XR_IF}" accept
        nft add rule inet xr_v6 fwd iifname "${XR_IF}" oifname "$i" accept
      fi
      for o in $outs6; do
        nft add rule inet xr_v6 fwd iifname "$i" oifname "$o" reject with icmpv6 admin-prohibited
      done
    done
  fi
  nft add rule inet xr_v6 fwd oifname "${XR_IF}" drop
  nft add rule inet xr_v6 fwd iifname "${XR_IF}" drop
}
