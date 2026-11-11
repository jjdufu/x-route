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

# 默认路由 / redirect-gateway 的分半路由不同步（由运行模式处理）
is_default_route() {
  case "$1" in
    default|0.0.0.0/0|0.0.0.0/1|128.0.0.0/1|::/0|::/1|8000::/1) return 0 ;;
  esac
  return 1
}

is_ip6_cidr() { case "$1" in *:*) return 0 ;; *) return 1 ;; esac; }

routes_valid() {
  up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  busy="${XR_NET_PREFIX}.0/24 127.0.0.0/8 169.254.0.0/16 ${XR_NET6}::/64"
  for i in $up $(tether_ifaces); do
    busy="$busy $(ip -4 -o addr show dev "$i" 2>/dev/null | awk '{print $4}')"
    busy="$busy $(ip -6 -o addr show dev "$i" 2>/dev/null | awk '{print $4}')"
  done
  n=0
  for c in $(routes_source | tr ', ' '\n'); do
    case "$c" in "") continue ;; esac
    is_default_route "$c" && continue
    nc="$(norm_route "$c")" || { log_err vpn "格式错误 $c"; continue; }
    is_default_route "$nc" && continue
    valid_cidr "$nc"   || { log_err vpn "格式错误 $c"; continue; }
    cidr_aligned "$nc" || { log_err vpn "主机位不为 0：$c"; continue; }
    ok=1
    for b in $busy; do
      [ -n "$b" ] || continue
      cidr_overlap "$nc" "$b" && { log_err vpn "$nc 与 $b 重叠"; ok=0; break; }
    done
    [ $ok = 1 ] || continue
    n=$((n+1)); [ $n -le 64 ] || { log_err vpn "超过 64 条，其余忽略"; break; }
    echo "$nc"
  done | sort -u
}

route_via() { if is_ip6_cidr "$1"; then echo "${XR_NET6}::254"; else echo "${XR_NET_PREFIX}.254"; fi; }
# $1=路由，其后为 ip 子命令；IPv6 走 ip -6
ipx() {
  _r="$1"; shift
  if is_ip6_cidr "$_r"; then ip6 "$@"; else ip "$@"; fi
}

route_add() {
  via="$(route_via "$1")"
  if [ "$(cat "${RUN}/net.mode" 2>/dev/null)" = ndc ]; then
    ndc network route add local xr0 "$1" "$via" >/dev/null 2>&1
  else
    ipx "$1" route replace "$1" via "$via" dev xr0 table 97
  fi
  ipx "$1" rule del pref 4801 to "$1" 2>/dev/null
  ipx "$1" rule add pref 4801 to "$1" lookup "$(lnt)"
}
route_del() {
  via="$(route_via "$1")"
  ndc network route remove local xr0 "$1" "$via" >/dev/null 2>&1
  ipx "$1" route del "$1" table 97 2>/dev/null
  ipx "$1" rule del pref 4801 to "$1" 2>/dev/null
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
    ipx "$c" route show table 97 2>/dev/null | grep -q "^$c via" || route_add "$c"
  done
}
routes_clear() {
  for c in $(cat "${RUN}/routes.active" 2>/dev/null); do route_del "$c"; done
  rm -f "${RUN}/routes.active"
}
