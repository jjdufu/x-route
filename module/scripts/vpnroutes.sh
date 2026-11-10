#!/system/bin/sh
# ==============================================================================
# x-route v2: 手工 VPN 网段（VPN_ROUTES）-> 手机路由 + 策略规则（docs/v2-plan/08-openvpn.md）
# 被 net.sh 引入；依赖 common.sh 的 CIDR 工具与 net.sh 的 tether_ifaces / lnt。
# VM 不向手机上报任何路由，网段只能在 WebUI 里手工填写。
# ==============================================================================

routes_valid() {  # 输出合法的 CIDR（每行一个）；不合法的记日志并丢弃
  up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  busy="${XR_NET_PREFIX}.0/24 127.0.0.0/8 169.254.0.0/16"
  for i in $up $(tether_ifaces); do
    busy="$busy $(ip -4 -o addr show dev "$i" 2>/dev/null | awk '{print $4}')"
  done
  n=0
  for c in $(echo "${VPN_ROUTES}" | tr ',' ' '); do
    valid_cidr "$c"   || { log_err vpn "格式错误 $c"; continue; }
    cidr_private "$c" || { log_err vpn "不是私网地址 $c"; continue; }
    cidr_aligned "$c" || { log_err vpn "主机位不为 0：$c"; continue; }
    ok=1
    for b in $busy; do cidr_overlap "$c" "$b" && { log_err vpn "$c 与 $b 重叠"; ok=0; break; }; done
    [ $ok = 1 ] || continue
    n=$((n+1)); [ $n -le 64 ] || { log_err vpn "超过 64 条，其余忽略"; break; }
    echo "$c"
  done | sort -u
}

route_add() {
  if [ "$(cat "${RUN}/net.mode" 2>/dev/null)" = ndc ]; then
    ndc network route add local xr0 "$1" "${XR_NET_PREFIX}.254" >/dev/null 2>&1
  else
    ip route replace "$1" via "${XR_NET_PREFIX}.254" dev xr0 table 97
  fi
  ip rule del pref 4801 to "$1" 2>/dev/null
  ip rule add pref 4801 to "$1" lookup "$(lnt)"
}
route_del() {
  ndc network route remove local xr0 "$1" "${XR_NET_PREFIX}.254" >/dev/null 2>&1
  ip route del "$1" table 97 2>/dev/null
  ip rule del pref 4801 to "$1" 2>/dev/null
}
routes_sync() {
  load_config
  want="$(routes_valid)"; have="$(cat "${RUN}/routes.active" 2>/dev/null)"
  for c in $have; do echo "$want" | grep -qx "$c" || route_del "$c"; done
  for c in $want; do echo "$have" | grep -qx "$c" || route_add "$c"; done
  echo "$want" > "${RUN}/routes.active"
}
routes_reassert() {
  for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
    ip route show table 97 2>/dev/null | grep -q "^$c via" || route_add "$c"
  done
}
routes_clear() {
  for c in $(cat "${RUN}/routes.active" 2>/dev/null); do route_del "$c"; done
  rm -f "${RUN}/routes.active"
}
