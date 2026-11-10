#!/system/bin/sh
# ==============================================================================
# x-route v2: 手机侧网络（docs/v2-plan/07-net.md）
# 用法：net.sh up|down|watch|sync|status
#   up    创建 xr0、登记 netd 本地网络、打开转发、建 XR_FWD/XR_NAT，并在后台启动 watch
#   down  逆序撤销全部改动（每一步都容错）
#   sync  按当前上游 / XR_EXPOSE / VPN_ROUTES 重建规则（幂等）
# ==============================================================================
MODDIR="${MODDIR:-/data/adb/modules/x_route}"
. "${MODDIR}/scripts/common.sh"
. "${MODDIR}/scripts/vpnroutes.sh"
load_config

XR_IF=xr0
CELL_RE='^(rmnet|ccmni|seth|v4-rmnet|v4-ccmni)'
NET="${XR_NET_PREFIX}.0/24"
# KernelSU 以 busybox ash 独立模式运行脚本，裸写 ip 会命中 busybox 的 ip 小程序
# （不支持 tuntap / 完整 rule 语法），因此 ip/ndc/iptables 一律走系统绝对路径
SYSBIN="${XR_SYSBIN:-/system/bin}"
ip()  { "${SYSBIN}/ip" "$@"; }
ndc() { "${SYSBIN}/ndc" "$@"; }
IPT="${SYSBIN}/iptables -w"
PROC="${XR_PROC:-/proc}"        # 测试时可指向临时目录
SYSNET="${XR_SYSNET:-/sys/class/net}"

ndc_ok() { ndc "$@" 2>&1 | grep -q '^200 '; }
lnt()    { [ "$(cat "${RUN}/net.mode" 2>/dev/null)" = ndc ] && echo local_network || echo 97; }
sysw()   { { echo "$2" > "$1"; } 2>/dev/null; }

# ---- 上游与热点网卡 ----
upstream_iface() {   # netd 默认网络规则：from all fwmark 0x0/0xffff iif lo lookup <iface>
  ip rule 2>/dev/null | sed -n 's/.*fwmark 0x0\/0xffff iif lo lookup \([^ ]*\).*/\1/p' | head -1
}
tether_ifaces() {    # netd 把热点、USB 共享等下游网卡登记在 local_network 表里
  ip route show table local_network 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1)}' | sort -u | grep -vx "${XR_IF}"
}
expose_ifaces() {
  case "${XR_EXPOSE}" in
    off) ;;
    hotspot) tether_ifaces ;;
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

net_up() {
  ensure_dirs
  ip link show "${XR_IF}" >/dev/null 2>&1 || ip tuntap add "${XR_IF}" mode tap || { log_err net "创建 ${XR_IF} 失败"; return 1; }
  ip addr flush dev "${XR_IF}"
  ip addr add "${XR_NET_PREFIX}.1/24" dev "${XR_IF}"
  sysw "${PROC}/sys/net/ipv6/conf/${XR_IF}/disable_ipv6" 1
  sysw "${PROC}/sys/net/ipv4/conf/${XR_IF}/rp_filter" 0
  ip link set "${XR_IF}" up

  # 1. 登记为 netd 本地网络；不可用时直接用 table 97
  if ndc_ok network interface add local "${XR_IF}" && ndc_ok network route add local "${XR_IF}" "${NET}"; then
    echo ndc > "${RUN}/net.mode"
  else
    echo fallback > "${RUN}/net.mode"
    ip route replace "${NET}" dev "${XR_IF}" table 97
    log_info net "ndc 不可用，改用 table 97，并由对账循环补回"
  fi
  ip rule del pref 4800 to "${NET}" 2>/dev/null
  ip rule add pref 4800 to "${NET}" lookup "$(lnt)"

  # 2. 转发
  if ! ndc_ok ipfwd enable xroute; then
    cat "${PROC}/sys/net/ipv4/ip_forward" > "${RUN}/ip_forward.orig"
    sysw "${PROC}/sys/net/ipv4/ip_forward" 1
  fi

  # 3. 链
  ${IPT} -N XR_FWD 2>/dev/null; ${IPT} -t nat -N XR_NAT 2>/dev/null
  fwd_first; nat_first
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

# ---- 根据当前状态重建上游、NAT 和 XR_FWD（幂等）----
# 标准路由器语义：手机当前上游（Wi-Fi、蜂窝或其他，不做区分）就是 VM 的 WAN，默认不信任：
#   - 出站：-i xr0 -o <上游出口> 放行，并在 XR_NAT 中 MASQUERADE；
#   - 回包：ESTABLISHED,RELATED 放行（排在所有 DROP 之前）；
#   - 入站：上游主动发往 xr0 的流量丢弃；只有 XR_EXPOSE 明确列出的下游
#     （hotspot：热点/USB 共享；all：再加非蜂窝上游所在局域网）可以访问 VM 网段。
sync_all() {
  load_config
  up="$(upstream_iface)"; outs="$(upstream_out "$up")"
  old_up="$(cat "${RUN}/net.up_if" 2>/dev/null)"; old_outs="$(cat "${RUN}/net.up_out" 2>/dev/null)"
  if [ "$up" != "$old_up" ] || [ "$outs" != "$old_outs" ] || [ ! -f "${RUN}/net.up_if" ]; then
    for o in $old_outs; do ndc ipfwd remove "${XR_IF}" "$o" >/dev/null 2>&1; done
    while ip rule del pref 18900 2>/dev/null; do :; done
    if [ -n "$up" ]; then
      fb=0
      for o in $outs; do ndc_ok ipfwd add "${XR_IF}" "$o" || fb=1; done
      [ $fb = 0 ] || ip rule add pref 18900 iif "${XR_IF}" lookup "$up"
    fi
    echo "$up" > "${RUN}/net.up_if"; echo "$outs" > "${RUN}/net.up_out"
    log_info net "上游 ${old_up:-无} -> ${up:-无}（出口 ${outs:-无}）"
  fi
  ${IPT} -t nat -F XR_NAT
  for o in $outs; do ${IPT} -t nat -A XR_NAT -s "${NET}" -o "$o" -j MASQUERADE; done

  ex="$(expose_ifaces | tr '\n' ' ')"; echo "$ex" > "${RUN}/net.expose_if"
  routes_sync      # 08：必须在 XR_FWD 重建前执行（XR_FWD 读取 routes.active）
  ${IPT} -F XR_FWD
  ${IPT} -A XR_FWD -o "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  ${IPT} -A XR_FWD -i "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  for o in $outs; do ${IPT} -A XR_FWD -i "${XR_IF}" -o "$o" -j ACCEPT; done
  for i in $ex; do
    ${IPT} -A XR_FWD -i "$i" -o "${XR_IF}" -d "${NET}" -j ACCEPT
    for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
      ${IPT} -A XR_FWD -i "$i" -o "${XR_IF}" -d "$c" -j ACCEPT
    done
  done
  for o in $outs; do   # WAN 入站（未被 XR_EXPOSE=all 放行的上游）显式丢弃
    case " $ex " in *" $o "*) ;; *) ${IPT} -A XR_FWD -i "$o" -o "${XR_IF}" -j DROP ;; esac
  done
  ${IPT} -A XR_FWD -o "${XR_IF}" -j DROP
  ${IPT} -A XR_FWD -i "${XR_IF}" -j DROP
  fwd_first
}

net_watch() {
  while [ -d "${SYSNET}/${XR_IF}" ]; do
    sleep 10; load_config
    up="$(upstream_iface)"; outs="$(upstream_out "$up")"; ex="$(expose_ifaces | tr '\n' ' ')"
    if [ "$up" != "$(cat "${RUN}/net.up_if" 2>/dev/null)" ] || [ "$outs" != "$(cat "${RUN}/net.up_out" 2>/dev/null)" ] \
       || [ "$ex" != "$(cat "${RUN}/net.expose_if" 2>/dev/null)" ]; then sync_all; fi
    fwd_first; nat_first
    ip rule 2>/dev/null | grep -q "^4800:.*to ${NET}" || ip rule add pref 4800 to "${NET}" lookup "$(lnt)"
    if [ "$(cat "${RUN}/net.mode" 2>/dev/null)" = fallback ]; then
      ip route show table 97 2>/dev/null | grep -q "^${NET} dev ${XR_IF}" || ip route replace "${NET}" dev "${XR_IF}" table 97
      routes_reassert
    fi
  done
}

net_down() {
  w="$(cat "${RUN}/net_watch.pid" 2>/dev/null)"; [ -n "$w" ] && kill "$w" 2>/dev/null
  routes_clear
  while ${IPT} -D FORWARD -j XR_FWD 2>/dev/null; do :; done
  ${IPT} -F XR_FWD 2>/dev/null; ${IPT} -X XR_FWD 2>/dev/null
  while ${IPT} -t nat -D POSTROUTING -j XR_NAT 2>/dev/null; do :; done
  ${IPT} -t nat -F XR_NAT 2>/dev/null; ${IPT} -t nat -X XR_NAT 2>/dev/null
  for o in $(cat "${RUN}/net.up_out" 2>/dev/null); do ndc ipfwd remove "${XR_IF}" "$o" >/dev/null 2>&1; done
  while ip rule del pref 18900 2>/dev/null; do :; done
  while ip rule del pref 4800 2>/dev/null; do :; done
  ndc ipfwd disable xroute >/dev/null 2>&1
  [ -f "${RUN}/ip_forward.orig" ] && sysw "${PROC}/sys/net/ipv4/ip_forward" "$(cat "${RUN}/ip_forward.orig")"
  ndc network route remove local "${XR_IF}" "${NET}" >/dev/null 2>&1
  ndc network interface remove local "${XR_IF}" >/dev/null 2>&1
  ip route flush table 97 dev "${XR_IF}" 2>/dev/null
  ip link del "${XR_IF}" 2>/dev/null
  rm -f "${RUN}"/net.* "${RUN}/routes.active" "${RUN}/ip_forward.orig" "${RUN}/net_watch.pid"
}

net_status() { # JSON 片段
  ex="$(cat "${RUN}/net.expose_if" 2>/dev/null)"; exj=""
  for i in $ex; do exj="${exj:+${exj},}\"$i\""; done
  printf '{"mode":"%s","up_if":"%s","expose":"%s","expose_if":[%s],"vm_ip":"%s.254","host_ip":"%s.1"}' \
    "$(cat "${RUN}/net.mode" 2>/dev/null || echo down)" "$(cat "${RUN}/net.up_if" 2>/dev/null)" \
    "${XR_EXPOSE}" "$exj" "${XR_NET_PREFIX}" "${XR_NET_PREFIX}"
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
