#!/system/bin/sh
# shellcheck disable=SC2034
# ==============================================================================
# XRouter: 热点交给 OpenWrt 分配地址（被 net.sh 引入，XR_HOTSPOT_BRIDGE=1 时生效）
# 虚拟机第二块网卡 eth1 对应手机侧 tap xr1；热点开着且虚拟机就绪时，建网桥 xrbr1 = xr1 + 热点网卡：
#   - OpenWrt eth1 接管安卓热点原来的网关地址与网段（如 10.142.10.47/24），提供 DHCP / DNS（IPv6 开时再发 RA / DHCPv6），
#     已连的设备续租时地址不变，网关 MAC 由 OpenWrt 逐个 ARP 刷新，无需重连；
#   - 热点设备的流量在二层直接进 OpenWrt，不再经过安卓转发和 BPF 加速；
#   - 安卓自己的 DHCP 应答与 IPv6 路由通告在热点网卡出口丢弃（ip/ip6tables OUTPUT），不和 OpenWrt 打架；
#   - 设备里残留的安卓 IPv6（前缀与默认路由）发到手机 MAC 的包落在 xrbr1 上，由 XR_FWD6 立即回 TCP RST /
#     ICMPv6 管理禁止，应用马上回落 IPv4，不再干等超时。
# xr0（手机 <-> 虚拟机三层链路）不进网桥，手机自身仍走规则 19100 / 表 233，守护进程与看门狗不受影响。
# 热点关闭、重开（网段会变）、虚拟机停止或关闭此选项时自动拆桥，热点交还安卓。
# 固定热点网段（XR_HOTSPOT_SUBNET，如 10.233.0.1/24）：OpenWrt eth1 用这个固定网关 / 网段，DHCP 只发这个网段，
#   不再跟随安卓每次随机的热点网段；安卓原网关地址作为第二地址保留在 eth1 上，已连设备续租前照常可用，
#   续租时被 dnsmasq（authoritative）拒绝后立即重新获取固定网段地址。
#   手机自身访问固定网段：规则 4790 -> 表 234 -> 经 xr0 交给 OpenWrt。
#   设置项为空 = 跟随安卓热点网段（旧行为）；固定网段与上游 / xr0 网段冲突时本次退回跟随安卓并记录错误。
# ==============================================================================
HS_BR=xrbr1
HS_TAP=xr1
HS_TABLE=234
HS_PREF=19050
HS_PREF4=4790    # 手机自身访问固定热点网段（在 4800 访问 xr0 网段之前，先于安卓的 fwmark 规则；4801 已被 VPN 路由占用）

hs_mac() { # 虚拟机第二块网卡的 MAC：XR_VM_MAC 末字节翻转最低位
  _l="${XR_VM_MAC##*:}"
  printf '%s:%02x\n' "${XR_VM_MAC%:*}" "$(( 0x$_l ^ 1 ))"
}

hs_tap_up() {
  ip link show "${HS_TAP}" >/dev/null 2>&1 || ip tuntap add "${HS_TAP}" mode tap || { log_err hotspot "创建 ${HS_TAP} 失败"; return 1; }
  ip addr flush dev "${HS_TAP}" 2>/dev/null
  sysw "${PROC}/sys/net/ipv6/conf/${HS_TAP}/disable_ipv6" 1
  ip link set "${HS_TAP}" up
}

hs_tap_down() {
  hs_unbridge
  ip link del "${HS_TAP}" 2>/dev/null
}

hs_master() { _m="$(readlink "${SYSNET}/$1/master" 2>/dev/null)"; echo "${_m##*/}"; }

# 候选热点网卡：安卓登记的下游网卡里第一个 Wi-Fi 网卡（已桥接时 local_network 中的路由仍在）
hs_pick() {
  for _i in $(cat "${RUN}/net.hs_if" 2>/dev/null) $(tether_ifaces); do
    case "$_i" in ""|"${XR_IF}"|"${HS_TAP}"|"${HS_BR}") continue ;; esac
    [ -d "${SYSNET}/$_i" ] || continue
    [ -d "${SYSNET}/$_i/wireless" ] || [ -d "${SYSNET}/$_i/phy80211" ] || [ -n "${XR_HS_ANY:-}" ] || continue
    [ "$(cat "${SYSNET}/$_i/operstate" 2>/dev/null)" = down ] && continue
    echo "$_i"; return 0
  done
  return 1
}

hs_guest_ok() { [ -f "${RUN}/guest/guest/hotspot" ]; }   # 新系统盘才上报，旧盘不支持时不桥接

hs_want() {
  [ "${XR_HOTSPOT_BRIDGE:-1}" = 1 ] && [ "${VM_NET:-1}" = 1 ] && vm_is_ready && [ -d "${SYSNET}/${HS_TAP}" ]
}

# $1=on <CIDR> [客户端IP…] | off  写给客侧，等其上报结果（最多 $2 秒）
hs_guest_cmd() {
  mkdir -p "${RUN}/guest/host"
  echo "$1 $(date +%s)" > "${RUN}/guest/host/hotspot.cmd"
  # 注意：sh 无局部变量，这里不能用 _want（hs_sync 的同名变量在拆桥后还要用来重新桥接）
  _gcw="$(echo "$1" | awk '{print $1, $2}' | sed 's/ $//')"
  _i=0
  while [ "$_i" -lt "${2:-10}" ]; do
    [ "$(cat "${RUN}/guest/guest/hotspot" 2>/dev/null)" = "$_gcw" ] && return 0
    sleep 1; _i=$((_i + 1))
  done
  return 1
}

hs_rules() { # $1=热点网卡：出口过滤安卓 DHCP 应答 / RA / DHCPv6；残留 IPv6 立即拒绝
  ${IPT} -N XR_HS 2>/dev/null; ${IPT} -F XR_HS
  ${IPT} -A XR_HS -o "$1" -p udp --sport 67 -j DROP
  ${IPT} -C OUTPUT -j XR_HS 2>/dev/null || ${IPT} -I OUTPUT 1 -j XR_HS
  ${IP6T} -N XR_HS6 2>/dev/null; ${IP6T} -F XR_HS6
  ${IP6T} -A XR_HS6 -o "$1" -p icmpv6 --icmpv6-type router-advertisement -j DROP
  ${IP6T} -A XR_HS6 -o "$1" -p udp --sport 547 -j DROP
  ${IP6T} -C OUTPUT -j XR_HS6 2>/dev/null || ${IP6T} -I OUTPUT 1 -j XR_HS6
  # 发到手机 MAC 的残留 IPv6：先路由到 xr0 方向（表 234），再由 XR_FWD6 第一条拒绝
  while ip6 rule del pref "${HS_PREF}" 2>/dev/null; do :; done
  ip6 route replace default dev "${XR_IF}" table "${HS_TABLE}"
  ip6 rule add pref "${HS_PREF}" iif "${HS_BR}" lookup "${HS_TABLE}"
}

hs_rules_clear() {
  while ip rule del pref "${HS_PREF4}" 2>/dev/null; do :; done
  ip route flush table "${HS_TABLE}" 2>/dev/null
  while ${IPT} -D OUTPUT -j XR_HS 2>/dev/null; do :; done
  ${IPT} -F XR_HS 2>/dev/null; ${IPT} -X XR_HS 2>/dev/null
  while ${IP6T} -D OUTPUT -j XR_HS6 2>/dev/null; do :; done
  ${IP6T} -F XR_HS6 2>/dev/null; ${IP6T} -X XR_HS6 2>/dev/null
  while ip6 rule del pref "${HS_PREF}" 2>/dev/null; do :; done
  ip6 route flush table "${HS_TABLE}" 2>/dev/null
}

# OpenWrt eth1 实际使用的网关 CIDR：$1=热点网卡 $2=安卓热点网关 CIDR
# 设了固定网段且不与上游 / xr0 冲突时用固定网段，否则跟随安卓热点网段
hs_gw_cidr() {
  _hf="${XR_HOTSPOT_SUBNET:-}"
  if [ -n "$_hf" ]; then
    if _hc="$(hs_subnet_clash "$_hf" "$1")"; then
      if [ "$(cat "${RUN}/net.hs_clash" 2>/dev/null)" != "$_hf $_hc" ]; then
        log_err hotspot "固定热点网段 ${_hf} 与 ${_hc} 冲突，本次热点网段跟随安卓"
        echo "$_hf $_hc" > "${RUN}/net.hs_clash"
      fi
      echo "$2"; return 0
    fi
    rm -f "${RUN}/net.hs_clash"
    echo "$_hf"; return 0
  fi
  rm -f "${RUN}/net.hs_clash"
  echo "$2"
}

# 手机自身访问固定热点网段：经 xr0 交给 OpenWrt（跟随安卓网段时不需要：安卓原网关地址仍在热点网卡上）
hs_route4() { # $1=OpenWrt 网关 CIDR $2=安卓热点网关 CIDR
  while ip rule del pref "${HS_PREF4}" 2>/dev/null; do :; done
  ip route flush table "${HS_TABLE}" 2>/dev/null
  [ "$1" = "$2" ] && return 0
  _hn="$(cidr_net "$1")"
  ip route replace "$_hn" via "${XR_NET_PREFIX}.254" dev "${XR_IF}" table "${HS_TABLE}"
  ip rule add pref "${HS_PREF4}" to "$_hn" lookup "${HS_TABLE}"
}

hs_bridge() { # $1=热点网卡
  _if="$1"
  _cidr="$(ip -4 -o addr show dev "$_if" 2>/dev/null | awk '{print $4; exit}')"
  [ -n "$_cidr" ] || { dlog hotspot "${_if} 还没有 IPv4 地址，暂不桥接"; return 1; }
  _gw="$(hs_gw_cidr "$_if" "$_cidr")"
  # 固定网段：安卓原网关作为 eth1 第二地址（+前缀），已连设备续租前不断网；网段相同则不需要
  _alias=""
  if [ "$_gw" != "$_cidr" ] && ! cidr_overlap "$_gw" "$_cidr"; then _alias="+${_cidr}"; fi
  _ll="$(ip -6 -o addr show dev "$_if" scope link 2>/dev/null | awk '{print $4; exit}')"
  _cl="$(ip -4 neigh show dev "$_if" 2>/dev/null | awk '$1 ~ /^[0-9.]+$/ && !/FAILED|INCOMPLETE/ {print $1}' | tr '\n' ' ')"
  dlog hotspot "准备桥接 ${_if}：安卓网关 ${_cidr}，OpenWrt 网关 ${_gw}${_alias:+（保留 ${_cidr} 给已连设备）}，链路本地 ${_ll:-无}，已知设备 ${_cl:-无}"
  # 1. OpenWrt 先接管网关地址并起 DHCP；失败则保持安卓热点不动
  if ! hs_guest_cmd "on ${_gw} ${_alias} ${_cl}" 15; then
    log_err hotspot "OpenWrt 未接管热点网段 ${_gw}（$(cat "${RUN}/guest/guest/hotspot" 2>/dev/null || echo 无响应)），热点保持由安卓分配"
    echo "$_if ${_cidr} ${_gw}" > "${RUN}/net.hs_fail"
    return 1
  fi
  # 2. 网桥：xr1 + 热点网卡；网桥不配 IPv4，arp_ignore=1 不替手机应答热点网关 ARP
  if ! ip link show "${HS_BR}" >/dev/null 2>&1; then
    ip link add "${HS_BR}" type bridge || { log_err hotspot "创建网桥失败"; hs_guest_cmd off 5; return 1; }
  fi
  ip link set "${HS_BR}" type bridge forward_delay 0 stp_state 0 2>/dev/null
  sysw "${PROC}/sys/net/ipv4/conf/${HS_BR}/arp_ignore" 1
  sysw "${PROC}/sys/net/ipv4/conf/${HS_BR}/rp_filter" 0
  sysw "${PROC}/sys/net/ipv6/conf/${HS_BR}/accept_ra" 0
  sysw "${PROC}/sys/net/ipv6/conf/${HS_BR}/disable_ipv6" 0
  ip link set "${HS_BR}" up
  ip link set "${HS_TAP}" master "${HS_BR}"
  ip link set "$_if" master "${HS_BR}" || { log_err hotspot "${_if} 无法加入网桥"; hs_unbridge; return 1; }
  sysw "${PROC}/sys/net/ipv6/conf/${HS_BR}/forwarding" 1
  # 设备仍把安卓的链路本地地址当路由器：网桥带上同一地址，才能接住并立即拒绝
  [ -n "$_ll" ] && ip6 addr replace "$_ll" dev "${HS_BR}" nodad 2>/dev/null
  hs_rules "$_if"
  hs_route4 "$_gw" "$_cidr"
  ${IP6T} -I XR_FWD6 1 -i "${HS_BR}" -j REJECT --reject-with icmp6-adm-prohibited
  ${IP6T} -I XR_FWD6 1 -i "${HS_BR}" -p tcp -j REJECT --reject-with tcp-reset
  v6_tether_offload_off
  echo "$_if" > "${RUN}/net.hs_if"; echo "$_cidr" > "${RUN}/net.hs_cidr"; echo "$_gw" > "${RUN}/net.hs_gw"; rm -f "${RUN}/net.hs_fail"
  if [ "$_gw" = "$_cidr" ]; then
    log_info hotspot "热点 ${_if} 已桥接到 OpenWrt（网关 ${_cidr} 由 OpenWrt 接管）"
  else
    log_info hotspot "热点 ${_if} 已桥接到 OpenWrt（固定网段 ${_gw}，安卓原网段 ${_cidr} 保留到设备续租）"
  fi
  diag_snap_bg "热点桥接 ${_if}"
  return 0
}

hs_unbridge() {
  _if="$(cat "${RUN}/net.hs_if" 2>/dev/null)"
  _cidr="$(cat "${RUN}/net.hs_cidr" 2>/dev/null)"
  [ -n "$_if" ] || [ -d "${SYSNET}/${HS_BR}" ] || { rm -f "${RUN}/net.hs_fail"; return 0; }
  hs_rules_clear
  while ${IP6T} -D XR_FWD6 -i "${HS_BR}" -p tcp -j REJECT --reject-with tcp-reset 2>/dev/null; do :; done
  while ${IP6T} -D XR_FWD6 -i "${HS_BR}" -j REJECT --reject-with icmp6-adm-prohibited 2>/dev/null; do :; done
  if [ -n "$_if" ] && [ "$(hs_master "$_if")" = "${HS_BR}" ]; then ip link set "$_if" nomaster; fi
  [ "$(hs_master "${HS_TAP}")" = "${HS_BR}" ] && ip link set "${HS_TAP}" nomaster
  ip link del "${HS_BR}" 2>/dev/null
  vm_alive && hs_guest_ok && hs_guest_cmd off 5
  # 安卓重新应答网关：免费 ARP 让设备立刻把网关 MAC 换回手机
  if [ -n "$_if" ] && [ -n "$_cidr" ] && [ -d "${SYSNET}/$_if" ] && [ -x "${BB}" ]; then
    ( "${BB}" arping -U -c 2 -I "$_if" -s "${_cidr%/*}" "${_cidr%/*}" ) </dev/null >/dev/null 2>&1 &
  fi
  rm -f "${RUN}/net.hs_if" "${RUN}/net.hs_cidr" "${RUN}/net.hs_gw" "${RUN}/net.hs_fail"
  [ -n "$_if" ] && log_info hotspot "热点 ${_if} 已交还安卓"
  diag_snap_bg "热点拆桥 ${_if}"
  return 0
}

# 对账：按选项 / 虚拟机状态 / 热点网卡与网段决定桥接或拆桥（幂等，对账循环每 10 秒调用）
hs_sync() {
  _want=""
  if hs_want; then
    if hs_guest_ok; then
      _want="$(hs_pick)"
    elif [ ! -f "${RUN}/net.hs_old" ]; then
      log_info hotspot "当前系统盘不支持热点接管（请重置系统盘或使用新版完整包），热点保持由安卓分配"
      : > "${RUN}/net.hs_old"
    fi
  fi
  _have="$(cat "${RUN}/net.hs_if" 2>/dev/null)"
  if [ -z "$_want" ]; then
    [ -n "$_have" ] || [ -d "${SYSNET}/${HS_BR}" ] || return 0
    dlog hotspot "拆桥：选项=${XR_HOTSPOT_BRIDGE:-1} 虚拟机=$(cat "${RUN}/state" 2>/dev/null || echo 未运行) 热点=$(tether_ifaces | tr '\n' ' ')"
    hs_unbridge
    return 0
  fi
  _cidr="$(ip -4 -o addr show dev "$_want" 2>/dev/null | awk '{print $4; exit}')"
  _hc="$(cat "${RUN}/net.hs_cidr" 2>/dev/null)"
  _hg="$(cat "${RUN}/net.hs_gw" 2>/dev/null)"
  # 已桥接网卡上的安卓地址可能已被桥接拿走：网段沿用记录值
  [ -n "$_cidr" ] || [ "$_want" != "$_have" ] || _cidr="$_hc"
  _gw="$(hs_gw_cidr "$_want" "$_cidr")"
  if [ "$_want" = "$_have" ] && [ "$(hs_master "$_want")" = "${HS_BR}" ] && [ "$(hs_master "${HS_TAP}")" = "${HS_BR}" ] \
     && [ "$_cidr" = "$_hc" ] && [ "$_gw" = "${_hg:-$_hc}" ]; then
    # 已桥接：只补回被清掉的过滤规则 / 固定网段路由
    if ! ${IPT} -C OUTPUT -j XR_HS 2>/dev/null || ! ${IP6T} -C OUTPUT -j XR_HS6 2>/dev/null; then hs_rules "$_want"; fi
    if [ "$_gw" != "$_cidr" ] && ! ip rule 2>/dev/null | grep -q "^${HS_PREF4}:"; then hs_route4 "$_gw" "$_cidr"; fi
    return 0
  fi
  # 同一网卡同一网段桥接失败过：不每 10 秒重试，等网段、网卡或固定网段设置变化
  [ "$(cat "${RUN}/net.hs_fail" 2>/dev/null)" = "$_want ${_cidr} ${_gw}" ] && return 0
  [ -n "$_have" ] && dlog hotspot "重新桥接：${_have}(${_hc} -> ${_hg:-$_hc}) -> ${_want}(${_cidr} -> ${_gw})"
  _hsw="$_want"   # hs_unbridge 内部会改写 _ 前缀变量
  [ -n "$_have" ] && hs_unbridge
  hs_bridge "$_hsw"
}

# XR_FWD6 头部：网桥收到的（发给手机 MAC 的残留 IPv6）一律立即拒绝
hs_fwd6_rules() {
  [ -f "${RUN}/net.hs_if" ] || return 0
  ${IP6T} -A XR_FWD6 -i "${HS_BR}" -p tcp -j REJECT --reject-with tcp-reset
  ${IP6T} -A XR_FWD6 -i "${HS_BR}" -j REJECT --reject-with icmp6-adm-prohibited
}
