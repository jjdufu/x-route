#!/system/bin/sh
# ==============================================================================
# XRouter: 手机侧网络（docs/v2-plan/07-net.md）
# 用法：net.sh up|down|watch|sync|status
#   up    创建 xr0、登记 netd 本地网络、打开转发、建 XR_FWD/XR_NAT，并在后台启动 watch
#   down  逆序撤销全部改动（每一步都容错）
#   sync  按当前上游 / XR_EXPOSE / VPN 路由 / 虚拟机 IPv6 状态重建规则（幂等）
# IPv6：xr0 上 ${XR_NET6}::1/64（手机）与 ::254（虚拟机）用于本机通信；出网不走 NAT66，
# 而从上游委托前缀中取地址交给虚拟机，手机做 ND 代理并转发。无 ip6tables nat 也能工作。
# ==============================================================================
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
. "${MODDIR}/scripts/common.sh"
. "${MODDIR}/scripts/vpnroutes.sh"
. "${MODDIR}/scripts/v6.sh"
. "${MODDIR}/scripts/traffic.sh"
. "${MODDIR}/scripts/diag.sh"
. "${MODDIR}/scripts/hotspot.sh"
load_config

XR_IF=xr0
CELL_RE='^(rmnet|ccmni|seth|v4-rmnet|v4-ccmni)'
NET="${XR_NET_PREFIX}.0/24"
# KernelSU 以 busybox ash 独立模式运行脚本，裸写 ip 会命中 busybox 的 ip 小程序
# （不支持 tuntap / 完整 rule 语法），因此 ip/ndc/iptables 一律走系统绝对路径
SYSBIN="${XR_SYSBIN:-/system/bin}"
# 全部经 xr_exec：调试模式下记录命令、返回值与错误输出（平时零开销直通）
ip()  { xr_exec "${SYSBIN}/ip" "$@"; }
ndc() { xr_exec "${SYSBIN}/ndc" "$@"; }
IPT="xr_exec ${SYSBIN}/iptables -w"
IP6T="xr_exec ${SYSBIN}/ip6tables -w"
ip6() { xr_exec "${SYSBIN}/ip" -6 "$@"; }
NET6="${XR_NET6}::/64"
PROC="${XR_PROC:-/proc}"        # 测试时可指向临时目录
SYSNET="${XR_SYSNET:-/sys/class/net}"

ndc_ok() { ndc "$@" 2>&1 | grep -q '^200 '; }
lnt()    { [ "$(cat "${RUN}/net.mode" 2>/dev/null)" = ndc ] && echo local_network || echo 97; }
sysw()   {
  if { echo "$2" > "$1"; } 2>/dev/null; then _swrc=0; else _swrc=1; fi
  dlog sysctl "rc=${_swrc} ${1#"${PROC}"/} = $2"
  return "$_swrc"
}

# ---- 上游与热点网卡 ----
upstream_iface() {   # netd 默认网络规则：from all fwmark 0x0/0xffff iif lo lookup <iface>
  ip rule 2>/dev/null | sed -n 's/.*fwmark 0x0\/0xffff iif lo lookup \([^ ]*\).*/\1/p' | head -1
}
tether_ifaces() {    # netd 把热点、USB 共享等下游网卡登记在 local_network 表里
  ip route show table local_network 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1)}' | sort -u | grep -vx "${XR_IF}"
}
expose_ifaces() {   # 管理访问：local 仅本机；lan 加热点设备；all 再加所连 Wi-Fi 局域网
  case "${XR_EXPOSE}" in
    local) ;;
    lan) tether_ifaces ;;
    all) { tether_ifaces; u="$(upstream_iface)"; [ -n "$u" ] && echo "$u"; } | sort -u ;;
  esac | grep -vE "${CELL_RE}"   # 运营商网络不是局域网：即使 all 也不把蜂窝当作暴露接口（行为与之前一致）
}

fwd_first() {   # 保证 XR_FWD 是 FORWARD 链的第一条
  [ "$(${IPT} -S FORWARD 2>/dev/null | sed -n 2p)" = "-A FORWARD -j XR_FWD" ] && return 0
  while ${IPT} -D FORWARD -j XR_FWD 2>/dev/null; do :; done
  ${IPT} -I FORWARD 1 -j XR_FWD
}
nat_first() {
  ${IPT} -t nat -C POSTROUTING -j XR_NAT 2>/dev/null || ${IPT} -t nat -I POSTROUTING 1 -j XR_NAT
}
fwd6_first() {
  [ "$(${IP6T} -S FORWARD 2>/dev/null | sed -n 2p)" = "-A FORWARD -j XR_FWD6" ] && return 0
  while ${IP6T} -D FORWARD -j XR_FWD6 2>/dev/null; do :; done
  ${IP6T} -I FORWARD 1 -j XR_FWD6
}
# 不再使用 ip6tables nat / NAT66；保留函数名以免旧调用报错，并顺手拆掉残留挂钩
nat6_first() {
  while ${IP6T} -t nat -D POSTROUTING -j XR_NAT6 2>/dev/null; do :; done
  return 0
}

net_up() {
  ensure_dirs
  ip link show "${XR_IF}" >/dev/null 2>&1 || ip tuntap add "${XR_IF}" mode tap || { log_err net "创建 ${XR_IF} 失败"; return 1; }
  ip addr flush dev "${XR_IF}"
  ip addr add "${XR_NET_PREFIX}.1/24" dev "${XR_IF}"
  sysw "${PROC}/sys/net/ipv6/conf/${XR_IF}/disable_ipv6" 0
  sysw "${PROC}/sys/net/ipv6/conf/${XR_IF}/accept_ra" 0
  sysw "${PROC}/sys/net/ipv4/conf/${XR_IF}/rp_filter" 0
  ip link set "${XR_IF}" up
  ip6 addr add "${XR_NET6}::1/64" dev "${XR_IF}" nodad
  hs_tap_up   # 虚拟机第二块网卡（热点网桥用），始终创建，切换选项不需要重启虚拟机

  # 1. 登记为 netd 本地网络；不可用时直接用 table 97
  if ndc_ok network interface add local "${XR_IF}" && ndc_ok network route add local "${XR_IF}" "${NET}"; then
    echo ndc > "${RUN}/net.mode"
    ndc network route add local "${XR_IF}" "${NET6}" >/dev/null 2>&1
  else
    echo fallback > "${RUN}/net.mode"
    ip route replace "${NET}" dev "${XR_IF}" table 97
    ip6 route replace "${NET6}" dev "${XR_IF}" table 97
    log_info net "ndc 不可用，改用 table 97，并由对账循环补回"
  fi
  ip rule del pref 4800 to "${NET}" 2>/dev/null
  ip rule add pref 4800 to "${NET}" lookup "$(lnt)"
  ip6 rule del pref 4800 to "${NET6}" 2>/dev/null
  ip6 rule add pref 4800 to "${NET6}" lookup "$(lnt)"

  # 2. 转发
  if ! ndc_ok ipfwd enable xrouter; then
    cat "${PROC}/sys/net/ipv4/ip_forward" > "${RUN}/ip_forward.orig"
    sysw "${PROC}/sys/net/ipv4/ip_forward" 1
  fi

  # 3. 链
  ${IPT} -N XR_FWD 2>/dev/null; ${IPT} -t nat -N XR_NAT 2>/dev/null
  fwd_first; nat_first
  rm -f "${RUN}/net.nat6"   # 旧版残留，IPv6 出网改为邻居代理，不再依赖 nat 表
  if ${IP6T} -N XR_FWD6 2>/dev/null || ${IP6T} -S XR_FWD6 >/dev/null 2>&1; then
    : > "${RUN}/net.fwd6"; fwd6_first
  elif v6_nft_setup; then
    log_info net "ip6tables filter 不可用，IPv6 转发改用 nft"
  else
    log_err net "ip6tables filter 与 nft 均不可用，IPv6 仅靠路由表拦截旁路"
  fi
  nat6_first
  rm -f "${RUN}/net.up_if"
  sync_all
  nohup sh "${MODDIR}/scripts/net.sh" watch >/dev/null 2>&1 &
  echo $! > "${RUN}/net_watch.pid"
  log_info net "xr0 ${XR_NET_PREFIX}.1/24 已就绪（$(cat "${RUN}/net.mode")）"
}

# 上游的 IPv4 实际出口：上游本身，加上其路由表里默认路由的出口网卡
# （纯 IPv6 蜂窝走 464xlat 时 IPv4 实际从 v4-rmnet_dataX 出去）
upstream_out() {
  [ -n "$1" ] || return 0
  { echo "$1"
    ip -4 route show table "$1" 2>/dev/null | awk '$1=="default"{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1)}'
  } | sort -u | tr '\n' ' '
}
# 上游的 IPv6 实际出口（464xlat 的 v4-* 只承载 IPv4，排除）
upstream_out6() {
  [ -n "$1" ] || return 0
  { echo "$1"
    ip6 route show table "$1" 2>/dev/null | awk '$1=="default"{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1)}'
  } | grep -v '^v4-' | sort -u | tr '\n' ' '
}


# ---- 运行模式：全局时（且 VM 已就绪）把本机与热点默认上网导入 VM；旁路不接管 ----
# 表 233 + pref 19100：默认路由经 172.29.233.254。pref 18900（iif xr0 走上游）恒在其前，防止环路。
# 不用 97/98/99：分别是 Android 的 local_network / legacy_network / legacy_system。
# VM 未就绪、已停止或切到旁路时立即撤掉，避免把手机自己的上网搞断。
GW_PREF=19100
GW_TABLE=233
# IPv6 拦截（block）时热点等下游的 IPv6：先导向 xr0 方向（表 235），再由 XR_FWD6 立即回 TCP RST / ICMPv6 管理禁止。
# 若沿用表 233 的 unreachable，转发包只能等内核限速的 ICMP，客户端要干等超时才回落 IPv4（打开网页明显变慢）。
# 手机本机的 IPv6 仍查表 233 的 unreachable：本机 socket 在路由阶段就同步失败，不需要 ICMP。
REJ6_PREF=19060
REJ6_TABLE=235

vm_alive() {
  p="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
  is_pid_running "$p"
}

vm_is_ready() {
  vm_alive || return 1
  s="$(cat "${RUN}/state" 2>/dev/null)"
  [ "$s" = ready ] || [ "$s" = stable ]
}

want_gateway() {
  [ "${XR_MODE:-global}" = global ] && vm_is_ready
}

# 虚拟机 IPv6 的实际状态由客侧（OpenWrt）写入控制目录，手机侧不另存
vm_v6() { [ "$(cat "${RUN}/guest/guest/ipv6" 2>/dev/null)" = 1 ]; }

# 全局接管状态：off 不接管；v6 IPv4/IPv6 都进虚拟机；block IPv4 进虚拟机、IPv6 直接拒绝（应用立刻回落 IPv4）
gw_want() {
  if ! want_gateway; then echo off
  elif vm_v6 && [ -f "${RUN}/net.v6proxy" ]; then echo v6
  else echo block; fi
}

# 调试：全局接管判断依据
gw_reason() {
  printf '模式=%s 虚拟机=%s 客侧IPv6=%s 邻居代理=%s' "${XR_MODE:-global}" \
    "$(vm_alive && cat "${RUN}/state" 2>/dev/null || echo 未运行)" \
    "$(cat "${RUN}/guest/guest/ipv6" 2>/dev/null || echo 未上报)" \
    "$([ -f "${RUN}/net.v6proxy" ] && echo 已安装 || echo 未安装)"
}

copy_dev_routes() {
  dev="$1"
  [ -n "$dev" ] && [ "$dev" != "${XR_IF}" ] || return 0
  ip -4 route show dev "$dev" 2>/dev/null | while IFS= read -r line; do
    [ -n "$line" ] || continue
    # shellcheck disable=SC2086
    set -- $line
    [ "$1" = "default" ] && continue
    # shellcheck disable=SC2086
    ip route replace $line dev "$dev" table "${GW_TABLE}" 2>/dev/null
  done
}

copy_dev_routes6() {   # 主表与 local_network 中该网卡的直连 / 前缀路由（不含默认路由）
  dev="$1"
  [ -n "$dev" ] && [ "$dev" != "${XR_IF}" ] || return 0
  for tb in main local_network; do
    ip6 route show table "$tb" dev "$dev" 2>/dev/null | while IFS= read -r line; do
      [ -n "$line" ] || continue
      # shellcheck disable=SC2086
      set -- $line
      case "$1" in default|unreachable|multicast|ff00::/8) continue ;; esac
      line="$(echo "$line" | sed 's/ expires [^ ]*//')"
      # shellcheck disable=SC2086
      ip6 route replace $line dev "$dev" table "${GW_TABLE}" 2>/dev/null
    done
  done
}

gw_install() {
  g="$1"
  ip route flush table "${GW_TABLE}" 2>/dev/null
  for d in $outs $(tether_ifaces); do copy_dev_routes "$d"; done
  ip route replace default via "${XR_NET_PREFIX}.254" dev "${XR_IF}" src "${XR_NET_PREFIX}.1" table "${GW_TABLE}"
  while ip rule del pref "${GW_PREF}" 2>/dev/null; do :; done
  ip rule add pref "${GW_PREF}" lookup "${GW_TABLE}"
  ip6 route flush table "${GW_TABLE}" 2>/dev/null
  for d in $outs6 $(tether_ifaces); do copy_dev_routes6 "$d"; done
  if [ "$g" = v6 ]; then
    ip6 route replace default via "${XR_NET6}::254" dev "${XR_IF}" src "${XR_NET6}::1" table "${GW_TABLE}"
  else
    ip6 route replace unreachable default table "${GW_TABLE}"
  fi
  while ip6 rule del pref "${GW_PREF}" 2>/dev/null; do :; done
  ip6 rule add pref "${GW_PREF}" lookup "${GW_TABLE}"
  while ip6 rule del pref "${REJ6_PREF}" 2>/dev/null; do :; done
  ip6 route flush table "${REJ6_TABLE}" 2>/dev/null
  if [ "$g" = block ]; then
    ip6 route replace default dev "${XR_IF}" table "${REJ6_TABLE}"
    for i in $(tether_ifaces); do
      [ "$i" = "${HS_BR}" ] && continue
      ip6 rule add pref "${REJ6_PREF}" iif "$i" lookup "${REJ6_TABLE}"
    done
  fi
  echo "$g" > "${RUN}/net.gw"
}

gw_remove() {
  while ip rule del pref "${GW_PREF}" 2>/dev/null; do :; done
  ip route flush table "${GW_TABLE}" 2>/dev/null
  while ip6 rule del pref "${GW_PREF}" 2>/dev/null; do :; done
  ip6 route flush table "${GW_TABLE}" 2>/dev/null
  while ip6 rule del pref "${REJ6_PREF}" 2>/dev/null; do :; done
  ip6 route flush table "${REJ6_TABLE}" 2>/dev/null
  rm -f "${RUN}/net.gw"
}

rules_set() {   # $1=ip|ip6：VM 出站 18800 查本地网络表、18900 走当前上游
  while $1 rule del pref 18800 2>/dev/null; do :; done
  while $1 rule del pref 18900 2>/dev/null; do :; done
  [ -n "$up" ] || return 0
  $1 rule add pref 18800 iif "${XR_IF}" lookup "$(lnt)"
  $1 rule add pref 18900 iif "${XR_IF}" lookup "$up"
}

# ---- 根据当前状态重建上游、NAT 和 XR_FWD（幂等）----
# 标准路由器语义：手机当前上游（Wi-Fi、蜂窝或其他，不做区分）就是 VM 的 WAN，默认不信任：
#   - 出站：-i xr0 -o <上游出口> 放行，并在 XR_NAT 中 MASQUERADE；
#   - 回包：ESTABLISHED,RELATED 放行（排在所有 DROP 之前）；
#   - 入站：上游主动发往 xr0 的流量丢弃；只有 XR_EXPOSE 明确列出的下游
#     （lan：热点/USB 共享；all：再加非蜂窝上游所在局域网）可以访问 VM 网段。
sync_all() {
  load_config
  up="$(upstream_iface)"; outs="$(upstream_out "$up")"; outs6="$(upstream_out6 "$up")"
  old_up="$(cat "${RUN}/net.up_if" 2>/dev/null)"; old_outs="$(cat "${RUN}/net.up_out" 2>/dev/null)"
  if [ "$up" != "$old_up" ] || [ "$outs" != "$old_outs" ] || [ ! -f "${RUN}/net.up_if" ]; then
    for o in $old_outs; do ndc ipfwd remove "${XR_IF}" "$o" >/dev/null 2>&1; done
    if [ -n "$up" ]; then
      for o in $outs; do ndc ipfwd add "${XR_IF}" "$o" >/dev/null 2>&1; done
    fi
    # ndc ipfwd 的 pref 21000 排在 19000/19100 之后，VM 出站会被导回 VM，必须由 18900 兜住；
    # 18800 先查本地网络表，VM 回热点等直连网段不走上游。IPv6 同构。
    rules_set ip; rules_set ip6
    echo "$up" > "${RUN}/net.up_if"; echo "$outs" > "${RUN}/net.up_out"
    log_info net "上游 ${old_up:-无} -> ${up:-无}（出口 ${outs:-无}）"
    dlog net "上游变化：${old_up:-无}(${old_outs:-无}) -> ${up:-无}(${outs:-无})，IPv6 出口 ${outs6:-无}"
    [ -n "${old_up}" ] && diag_snap_bg "上游变化 ${old_up} -> ${up:-无}"
  fi
  ${IPT} -t nat -F XR_NAT
  for o in $outs; do ${IPT} -t nat -A XR_NAT -s "${NET}" -o "$o" -j MASQUERADE; done

  ex="$(expose_ifaces | tr '\n' ' ')"; echo "$ex" > "${RUN}/net.expose_if"
  routes_sync      # 08：必须在 XR_FWD 重建前执行（XR_FWD 读取 routes.active）
  acct_setup "$([ -f "${RUN}/net.fwd6" ] && echo 1)"   # 流量统计：内网列表同样取自 routes.active
  ${IPT} -F XR_FWD
  acct_present && ${IPT} -A XR_FWD -j XR_ACCT   # 只计数后 RETURN，不影响后续放行 / 丢弃
  ${IPT} -A XR_FWD -o "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  ${IPT} -A XR_FWD -i "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  for o in $outs; do ${IPT} -A XR_FWD -i "${XR_IF}" -o "$o" -j ACCEPT; done
  for i in $ex; do
    ${IPT} -A XR_FWD -i "$i" -o "${XR_IF}" -d "${NET}" -j ACCEPT
    for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
      case "$c" in *:*) continue ;; esac
      ${IPT} -A XR_FWD -i "$i" -o "${XR_IF}" -d "$c" -j ACCEPT
    done
  done
  for o in $outs; do   # WAN 入站（未被 XR_EXPOSE=all 放行的上游）显式丢弃
    case " $ex " in *" $o "*) ;; *) ${IPT} -A XR_FWD -i "$o" -o "${XR_IF}" -j DROP ;; esac
  done
  if want_gateway; then
    for i in $(tether_ifaces); do
      ${IPT} -A XR_FWD -i "$i" -o "${XR_IF}" -j ACCEPT
      ${IPT} -A XR_FWD -i "${XR_IF}" -o "$i" -j ACCEPT
      for o in $outs; do ${IPT} -A XR_FWD -i "$i" -o "$o" -j DROP; done
    done
  fi
  ${IPT} -A XR_FWD -o "${XR_IF}" -j DROP
  ${IPT} -A XR_FWD -i "${XR_IF}" -j DROP
  fwd_first
  hs_sync
  v6_proxy_sync
  g="$(gw_want)"
  dlog net "同步：上游=${up:-无} 出口=${outs:-无} IPv6出口=${outs6:-无} 暴露=${ex:-无} 热点=$(tether_ifaces | tr '\n' ' ') 接管=${g}（$(gw_reason)）"
  case "$g" in block) dlog net "IPv6 拦截：虚拟机 IPv6 $(vm_v6 && echo 已开但邻居代理不可用 || echo 已关闭)，手机与热点的 IPv6 出网被拒绝，应用回落 IPv4" ;; esac
  sync_v6 "$g"
  if [ "$g" = off ]; then gw_remove; else gw_install "$g"; fi
}

# IPv6 转发（filter / nft）：开关关闭时热点 IPv6 也不绕过虚拟机（全局下被拒绝）
sync_v6() {
  echo "$outs6" > "${RUN}/net.up_out6"; { vm_v6 && echo 1; } > "${RUN}/net.v6"
  ${IP6T} -F XR_FWD6
  acct_present6 && ${IP6T} -A XR_FWD6 -j XR_ACCT6
  hs_fwd6_rules
  ${IP6T} -A XR_FWD6 -o "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  ${IP6T} -A XR_FWD6 -i "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  if vm_v6; then   # 虚拟机自身 IPv6 出网（旁路模式同样可用）
    for o in $outs6; do ${IP6T} -A XR_FWD6 -i "${XR_IF}" -o "$o" -j ACCEPT; done
  fi
  for i in $ex; do
    ${IP6T} -A XR_FWD6 -i "$i" -o "${XR_IF}" -d "${NET6}" -j ACCEPT
    for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
      case "$c" in *:*) ${IP6T} -A XR_FWD6 -i "$i" -o "${XR_IF}" -d "$c" -j ACCEPT ;; esac
    done
  done
  for o in $outs6; do
    case " $ex " in *" $o "*) ;; *) ${IP6T} -A XR_FWD6 -i "$o" -o "${XR_IF}" -j DROP ;; esac
  done
  if [ "$1" != off ]; then
    for i in $(tether_ifaces); do
      if [ "$1" = v6 ]; then
        ${IP6T} -A XR_FWD6 -i "$i" -o "${XR_IF}" -j ACCEPT
        ${IP6T} -A XR_FWD6 -i "${XR_IF}" -o "$i" -j ACCEPT
      else
        # 拦截：TCP 回 RST（不受 ICMP 限速），其余回管理禁止，客户端立即回落 IPv4
        ${IP6T} -A XR_FWD6 -i "$i" -o "${XR_IF}" -p tcp -j REJECT --reject-with tcp-reset
        ${IP6T} -A XR_FWD6 -i "$i" -o "${XR_IF}" -j REJECT --reject-with icmp6-adm-prohibited
      fi
      for o in $outs6; do
        ${IP6T} -A XR_FWD6 -i "$i" -o "$o" -p tcp -j REJECT --reject-with tcp-reset
        ${IP6T} -A XR_FWD6 -i "$i" -o "$o" -j REJECT --reject-with icmp6-adm-prohibited
      done
    done
  fi
  ${IP6T} -A XR_FWD6 -o "${XR_IF}" -j DROP
  ${IP6T} -A XR_FWD6 -i "${XR_IF}" -j DROP
  fwd6_first
  v6_nft_sync "$1"
}

net_watch() {
  while [ -d "${SYSNET}/${XR_IF}" ]; do
    sleep 10; load_config
    up="$(upstream_iface)"; outs="$(upstream_out "$up")"; ex="$(expose_ifaces | tr '\n' ' ')"
    have_gw="$(cat "${RUN}/net.gw" 2>/dev/null || echo off)"
    want_gw="$(gw_want)"
    ck="$(cksum "${RUN}/guest/guest/openvpn.routes" 2>/dev/null || echo none)"
    if [ "$up" != "$(cat "${RUN}/net.up_if" 2>/dev/null)" ] || [ "$outs" != "$(cat "${RUN}/net.up_out" 2>/dev/null)" ] \
       || [ "$ex" != "$(cat "${RUN}/net.expose_if" 2>/dev/null)" ] \
       || [ "$want_gw" != "$have_gw" ] || [ "$(upstream_out6 "$up")" != "$(cat "${RUN}/net.up_out6" 2>/dev/null)" ] \
       || [ "$(vm_v6 && echo 1)" != "$(cat "${RUN}/net.v6" 2>/dev/null)" ] \
       || [ "$(v6_src_fingerprint)" != "$(cat "${RUN}/net.v6src" 2>/dev/null)" ] \
       || [ "$ck" != "$(cat "${RUN}/vpn.cksum" 2>/dev/null)" ]; then
      dlog watch "状态变化触发同步：上游 $(cat "${RUN}/net.up_if" 2>/dev/null)->${up} 接管 ${have_gw}->${want_gw} 暴露 [$(cat "${RUN}/net.expose_if" 2>/dev/null)]->[${ex}]"
      sync_all
    fi
    fwd_first; nat_first; fwd6_first; nat6_first; acct_hooks
    hs_sync
    ip rule 2>/dev/null | grep -q "^4800:.*to ${NET}" || ip rule add pref 4800 to "${NET}" lookup "$(lnt)"
    ip6 rule 2>/dev/null | grep -q "^4800:.*to ${NET6}" || ip6 rule add pref 4800 to "${NET6}" lookup "$(lnt)"
    for f in ip ip6; do
      [ -z "$up" ] || $f rule 2>/dev/null | grep -q "^18800:.*iif ${XR_IF}" || $f rule add pref 18800 iif "${XR_IF}" lookup "$(lnt)"
      [ -z "$up" ] || $f rule 2>/dev/null | grep -q "^18900:.*iif ${XR_IF} lookup ${up}" || $f rule add pref 18900 iif "${XR_IF}" lookup "$up"
    done
    if [ "$(cat "${RUN}/net.mode" 2>/dev/null)" = fallback ]; then
      ip route show table 97 2>/dev/null | grep -q "^${NET} dev ${XR_IF}" || ip route replace "${NET}" dev "${XR_IF}" table 97
      ip6 route show table 97 2>/dev/null | grep -q "^${NET6} dev ${XR_IF}" || ip6 route replace "${NET6}" dev "${XR_IF}" table 97
      routes_reassert
    fi
  done
}

net_down() {
  w="$(cat "${RUN}/net_watch.pid" 2>/dev/null)"; [ -n "$w" ] && kill "$w" 2>/dev/null
  hs_tap_down
  v6_proxy_remove
  v6_nft_teardown
  gw_remove
  routes_clear
  while ${IPT} -D FORWARD -j XR_FWD 2>/dev/null; do :; done
  ${IPT} -F XR_FWD 2>/dev/null; ${IPT} -X XR_FWD 2>/dev/null
  while ${IPT} -t nat -D POSTROUTING -j XR_NAT 2>/dev/null; do :; done
  ${IPT} -t nat -F XR_NAT 2>/dev/null; ${IPT} -t nat -X XR_NAT 2>/dev/null
  for o in $(cat "${RUN}/net.up_out" 2>/dev/null); do ndc ipfwd remove "${XR_IF}" "$o" >/dev/null 2>&1; done
  while ${IP6T} -D FORWARD -j XR_FWD6 2>/dev/null; do :; done
  ${IP6T} -F XR_FWD6 2>/dev/null; ${IP6T} -X XR_FWD6 2>/dev/null
  while ${IP6T} -t nat -D POSTROUTING -j XR_NAT6 2>/dev/null; do :; done
  ${IP6T} -t nat -F XR_NAT6 2>/dev/null; ${IP6T} -t nat -X XR_NAT6 2>/dev/null
  acct_teardown    # XR_FWD / XR_FWD6 已删，计数折算保存后再删统计链
  for f in ip ip6; do
    for p in 18800 18900 4800; do while $f rule del pref "$p" 2>/dev/null; do :; done; done
  done
  ndc ipfwd disable xrouter >/dev/null 2>&1
  [ -f "${RUN}/ip_forward.orig" ] && sysw "${PROC}/sys/net/ipv4/ip_forward" "$(cat "${RUN}/ip_forward.orig")"
  ndc network route remove local "${XR_IF}" "${NET}" >/dev/null 2>&1
  ndc network route remove local "${XR_IF}" "${NET6}" >/dev/null 2>&1
  ndc network interface remove local "${XR_IF}" >/dev/null 2>&1
  ip route flush table 97 dev "${XR_IF}" 2>/dev/null
  ip6 route flush table 97 dev "${XR_IF}" 2>/dev/null
  ip link del "${XR_IF}" 2>/dev/null
  rm -f "${RUN}"/net.* "${RUN}/routes.active" "${RUN}/ip_forward.orig" "${RUN}/net_watch.pid"
}

net_status() { # JSON 片段
  ex="$(cat "${RUN}/net.expose_if" 2>/dev/null)"; exj=""
  for i in $ex; do exj="${exj:+${exj},}\"$i\""; done
  printf '{"mode":"%s","up_if":"%s","expose":"%s","expose_if":[%s],"vm_ip":"%s.254","host_ip":"%s.1","gw":"%s","hotspot":"%s"}' \
    "$(cat "${RUN}/net.mode" 2>/dev/null || echo down)" "$(cat "${RUN}/net.up_if" 2>/dev/null)" \
    "${XR_EXPOSE}" "$exj" "${XR_NET_PREFIX}" "${XR_NET_PREFIX}" "$(cat "${RUN}/net.gw" 2>/dev/null || echo off)" \
    "$(cat "${RUN}/net.hs_if" 2>/dev/null)"
}

if [ "${0##*/}" = net.sh ]; then
  case "$1" in
    up) net_up ;;
    down) net_down ;;
    watch) net_watch ;;
    sync) [ -d "${SYSNET}/${XR_IF}" ] && sync_all ;;
    status) net_status; echo ;;
    *) echo "usage: net.sh up|down|watch|sync|status"; exit 2 ;;
  esac
fi
