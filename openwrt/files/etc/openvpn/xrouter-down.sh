#!/bin/sh
# XRouter: OpenVPN down（所有实例共用）。tun 网卡此时可能还在，稍后由 xrouter-vpn 的 2 秒轮询再刷新一次；
# 这里先把本隧道的记录清掉，避免上报已断开的隧道。
dev="${dev:-tun0}"
rm -f "/tmp/xrouter-tun/${dev}.proto" "/tmp/xrouter-tun/${dev}.name"
( sleep 1; /usr/libexec/xrouter-vpnstat; /usr/libexec/xrouter-tunmtu apply ) </dev/null >/dev/null 2>&1 &
if [ ! -d /var/run/xrouter ]; then
  for d in /dev/hvc0 /dev/ttyS0; do
    [ -c "$d" ] || continue
    echo "XR_VPN_STATUS down XR_VPN_ROUTES" > "$d" 2>/dev/null && break
  done
fi
exit 0
