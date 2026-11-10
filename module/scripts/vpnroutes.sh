#!/system/bin/sh
# XRouter: 把 OpenVPN 实际生效的路由（客侧上报）同步到手机，不解析 ovpn，不使用手工网段。
# 被 net.sh 引入。默认路由 / redirect-gateway 不同步（全局模式已由运行模式处理）。

guest_routes_file() { echo "${RUN}/guest/guest/openvpn.routes"; }

# 控制通道回退：从 vm-ctl.log 取最后一次 XR_VPN_ROUTES
scrape_ctl_routes() {
  f="${RUN}/vm-ctl.log"
  [ -f "$f" ] || return 0
  sed -n 's/.*XR_VPN_ROUTES[[:space:]]*//p' "$f" | tail -1
}

routes_source() {
  gf="$(guest_routes_file)"
  if [ -f "$gf" ]; then
    cat "$gf" 2>/dev/null
    return 0
  fi
  scrape_ctl_routes | tr ' ' '\n'
}

routes_valid() {
  up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  busy="${XR_NET_PREFIX}.0/24 127.0.0.0/8 169.254.0.0/16"
  for i in $up $(tether_ifaces); do
    busy="$busy $(ip -4 -o addr show dev "$i" 2>/dev/null | awk '{print $4}')"
  done
  n=0
  for c in $(routes_source | tr ', ' '\n'); do
    case "$c" in
      ""|default|0.0.0.0/0|0.0.0.0/1|128.0.0.0/1) continue ;;
    esac
    valid_cidr "$c"   || { log_err vpn "格式错误 $c"; continue; }
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
  cksum "$(guest_routes_file)" 2>/dev/null > "${RUN}/vpn.cksum" || echo none > "${RUN}/vpn.cksum"
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
