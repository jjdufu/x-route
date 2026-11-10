#!/bin/sh
# XRouter: OpenVPN down。清空已上报的路由与状态。
ST="/var/run/xrouter/guest"
if [ ! -d /var/run/xrouter ]; then
  ST="/tmp/xrouter-guest"
fi
mkdir -p "$ST"
echo down > "$ST/openvpn.status"
: > "$ST/openvpn.addr"
: > "$ST/openvpn.routes"
: > "$ST/openvpn.if"
if [ ! -d /var/run/xrouter ]; then
  for d in /dev/hvc0 /dev/ttyS0; do
    [ -c "$d" ] || continue
    echo "XR_VPN_STATUS down XR_VPN_ROUTES" > "$d" 2>/dev/null && break
  done
fi
