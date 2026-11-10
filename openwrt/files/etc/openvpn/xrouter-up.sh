#!/bin/sh
# XRouter: OpenVPN up / route-up。只记录实际生效的路由，不解析 ovpn 文件。
ST="/var/run/xrouter/guest"
if [ ! -d /var/run/xrouter ]; then
  ST="/tmp/xrouter-guest"
fi
mkdir -p "$ST"
dev="${dev:-tun0}"
echo up > "$ST/openvpn.status"
echo "${ifconfig_local:-}" > "$ST/openvpn.addr"
echo "${dev}" > "$ST/openvpn.if"
: > "$ST/openvpn.routes"
ip -4 route show dev "$dev" 2>/dev/null | awk '{print $1}' | while read -r c; do
  case "$c" in
    ""|default|0.0.0.0/0|0.0.0.0/1|128.0.0.0/1) continue ;;
    *) echo "$c" >> "$ST/openvpn.routes" ;;
  esac
done
if [ ! -d /var/run/xrouter ]; then
  routes="$(tr '\n' ' ' < "$ST/openvpn.routes")"
  msg="XR_VPN_STATUS up XR_VPN_ADDR ${ifconfig_local:-} XR_VPN_ROUTES ${routes}"
  for d in /dev/hvc0 /dev/ttyS0; do
    [ -c "$d" ] || continue
    echo "$msg" > "$d" 2>/dev/null && break
  done
fi
