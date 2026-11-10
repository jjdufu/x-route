#!/bin/sh
# XRouter: 在 VM（ImmortalWrt）里执行的能力核对脚本，由 tests/e2e-qemu.sh 通过 ssh 调用（不需要联网）。
# 核对 OpenVPN 与 Clash 类插件（OpenClash / nikki 等）全功能所需的内核能力，全部通过时输出 CAPS_OK。
bad=0; ok() { echo "ok: $*"; }; no() { echo "FAIL: $*"; bad=1; }
# 1. 预装的官方 kmod 能 modprobe（内核与官方 =m 划分一致，kmodloader 依赖能解析）
for m in tun nf_tables nf_conntrack nf_conntrack_netlink nft_ct nft_nat nft_masq nft_redir nft_fib_inet \
         nft_tproxy nft_socket nf_tproxy_ipv4 nf_tproxy_ipv6 nf_socket_ipv4 nf_socket_ipv6 nft_fullcone \
         nf_flow_table_inet nft_flow_offload inet_diag tcp_diag udp_diag ovpn; do
  modprobe "$m" 2>/dev/null
  if grep -q "^$m " /proc/modules || [ -d "/sys/module/$m" ]; then ok "module $m"; else no "module $m"; fi
done
# 2. Clash 常用的 nft 规则：socket / tproxy（v4+v6）/ redirect / masquerade / fullcone / numgen / jhash / ct mark
if nft -f - <<'N'
table inet xrcaps {
  chain c_pre { type filter hook prerouting priority mangle; policy accept;
    meta l4proto { tcp, udp } socket transparent 1 meta mark set 0x1 accept
    meta l4proto tcp ip daddr 198.18.0.0/16 tproxy ip to 127.0.0.1:7893 meta mark set 0x1 accept
    meta l4proto udp ip6 daddr fd00::/8 tproxy ip6 to [::1]:7893 meta mark set 0x1 accept
  }
  chain c_dn { type nat hook prerouting priority dstnat; policy accept;
    tcp dport 53 redirect to :1053
    udp dport 53 redirect to :1053
  }
  chain c_sn { type nat hook postrouting priority srcnat; policy accept;
    oifname "xrcaps9" masquerade
    oifname "xrcaps8" fullcone
  }
  chain c_out { type route hook output priority mangle; policy accept;
    meta mark set numgen inc mod 2
    meta mark set jhash ip saddr mod 4
    ct mark set meta mark
  }
}
N
then ok "nft tproxy/socket/redirect/masq/fullcone rules"; else no "nft rules"; fi
nft delete table inet xrcaps 2>/dev/null
# 3. 策略路由（fwmark + 多路由表，v4/v6）
if ip rule add fwmark 0x1 table 100 pref 900 && ip route add local 0.0.0.0/0 dev lo table 100 \
   && ip -6 rule add fwmark 0x1 table 100 pref 900 && ip -6 route add local ::/0 dev lo table 100; then
  ok "policy routing"; else no "policy routing"; fi
ip rule del pref 900 2>/dev/null; ip -6 rule del pref 900 2>/dev/null; ip route flush table 100 2>/dev/null; ip -6 route flush table 100 2>/dev/null
# 4. TUN 设备
if ip tuntap add mode tun dev xrcaps0 && ip link set xrcaps0 up; then ok "tun"; else no "tun"; fi
ip link del xrcaps0 2>/dev/null
# 5. IPv6 转发可开
if [ -w /proc/sys/net/ipv6/conf/all/forwarding ]; then ok "ipv6 forwarding"; else no "ipv6 forwarding"; fi
# 6. fw4 fullcone + flow offloading 可由用户/插件开启（默认关闭），验证后恢复
uci -q set firewall.@defaults[0].fullcone=1; uci -q set firewall.@defaults[0].flow_offloading=1
if fw4 -q check && fw4 -q reload 2>/dev/null && nft list ruleset | grep -q fullcone && nft list ruleset | grep -q "flow add"; then
  ok "fw4 fullcone/offload"; else no "fw4 fullcone/offload"; fi
uci revert firewall; fw4 -q reload 2>/dev/null
# 7. 内核日志里不应出现模块兼容问题
if dmesg | grep -qiE "duplicate symbol|unknown symbol|disagrees about version|missing dependency"; then no "dmesg module errors"; else ok "dmesg clean"; fi
if openvpn --version | head -1 | grep -q DCO; then ok "openvpn DCO"; else no "openvpn DCO"; fi
[ "$bad" = 0 ] && echo CAPS_OK
