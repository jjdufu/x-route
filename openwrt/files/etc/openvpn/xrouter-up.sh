#!/bin/sh
# XRouter: OpenVPN up / route-up（所有实例共用，不写死 tun0）。不解析 ovpn 文件：
#   记下本隧道的外层协议与实例名，按上游 MTU 重算全部隧道的 MTU / MSS（xrouter-tunmtu），再上报实际状态（xrouter-vpnstat）。
dev="${dev:-tun0}"
case "${proto_1:-udp}" in
  tcp*) p=tcp ;;
  *) p=udp ;;
esac
fam=4
[ -n "${trusted_ip6:-}" ] && fam=6
case "${proto_1:-}" in *6*) fam=6 ;; esac
p="${p}${fam}"
mkdir -p /tmp/xrouter-tun
n="${config:-}"; n="${n##*/}"; n="${n%.conf}"; n="${n%.ovpn}"
[ -n "$n" ] && echo "$n" > "/tmp/xrouter-tun/${dev}.name"
/usr/libexec/xrouter-tunmtu proto "$dev" "$p"
/usr/libexec/xrouter-tunmtu apply
/usr/libexec/xrouter-vpnstat
if [ ! -d /var/run/xrouter ]; then
  ST=/tmp/xrouter-guest
  routes="$(tr '\n' ' ' < "$ST/openvpn.routes" 2>/dev/null)"
  msg="XR_VPN_STATUS up XR_VPN_ADDR ${ifconfig_local:-} XR_VPN_ROUTES ${routes}"
  for d in /dev/hvc0 /dev/ttyS0; do
    [ -c "$d" ] || continue
    echo "$msg" > "$d" 2>/dev/null && break
  done
fi
exit 0
