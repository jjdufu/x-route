# 07 `module/scripts/net.sh`：手机侧网络

用法：`net.sh up|down|watch|sync|status`。`up` 会顺带在后台启动 `watch`，`down` 会停掉它。

## 状态文件（都在 `${RUN}/`）

| 文件 | 内容 |
|---|---|
| `net.mode` | `ndc` 或 `fallback`（ndc 不可用时直接操作 table 97） |
| `net.up_if` | 当前上游网卡 |
| `net.expose_if` | 当前放行的入口网卡列表 |
| `routes.active` | 当前已生效的 VPN 手工网段（08） |
| `net_watch.pid` | 对账循环的 pid |

## 常量与工具

```sh
XR_IF=xr0
CELL_RE='^(rmnet|rmnet_data|ccmni|seth|v4-rmnet)'
NET="${XR_NET_PREFIX}.0/24"
ndc_ok() { ndc "$@" 2>&1 | grep -q '^200 '; }
lnt()    { [ "$(cat "${RUN}/net.mode")" = ndc ] && echo local_network || echo 97; }
```

## 上游与热点网卡

```sh
upstream_iface() {   # netd 默认网络规则：from all fwmark 0x0/0xffff iif lo lookup <iface>
  ip rule | sed -n 's/.*fwmark 0x0\/0xffff iif lo lookup \([^ ]*\).*/\1/p' | head -1
}
tether_ifaces() {    # netd 把热点、USB 网络共享等下游网卡登记在 local_network 表里
  ip route show table local_network 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1)}' | sort -u | grep -vx "${XR_IF}"
}
expose_ifaces() {
  case "${XR_EXPOSE}" in
    off) ;;
    hotspot) tether_ifaces ;;
    all) { tether_ifaces; u="$(upstream_iface)"; echo "$u" | grep -qE "${CELL_RE}" || echo "$u"; } | sort -u ;;
  esac | grep -vE "${CELL_RE}"
}
```

## `net_up`

```sh
net_up() {
  load_config
  ip link show "${XR_IF}" >/dev/null 2>&1 || ip tuntap add "${XR_IF}" mode tap
  ip addr flush dev "${XR_IF}"
  ip addr add "${XR_NET_PREFIX}.1/24" dev "${XR_IF}"
  echo 1 > /proc/sys/net/ipv6/conf/${XR_IF}/disable_ipv6
  echo 0 > /proc/sys/net/ipv4/conf/${XR_IF}/rp_filter
  ip link set "${XR_IF}" up

  # 1. 登记为 netd 本地网络
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
  ndc_ok ipfwd enable xroute || { cat /proc/sys/net/ipv4/ip_forward > "${RUN}/ip_forward.orig"; echo 1 > /proc/sys/net/ipv4/ip_forward; }

  # 3. 链
  iptables -N XR_FWD 2>/dev/null; iptables -t nat -N XR_NAT 2>/dev/null
  fwd_first
  iptables -t nat -C POSTROUTING -j XR_NAT 2>/dev/null || iptables -t nat -I POSTROUTING 1 -j XR_NAT

  sync_all
  sh "${MODDIR}/scripts/net.sh" watch >/dev/null 2>&1 &
  echo $! > "${RUN}/net_watch.pid"
}
fwd_first() {   # 保证 XR_FWD 是 FORWARD 链的第一条
  [ "$(iptables -S FORWARD | sed -n 2p)" = "-A FORWARD -j XR_FWD" ] && return
  while iptables -D FORWARD -j XR_FWD 2>/dev/null; do :; done
  iptables -I FORWARD 1 -j XR_FWD
}
```

## `sync_all`：根据当前状态重建上游、NAT 和 XR_FWD（幂等）

```sh
sync_all() {
  load_config
  up="$(upstream_iface)"; old_up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  if [ "$up" != "$old_up" ]; then
    [ -n "$old_up" ] && ndc ipfwd remove "${XR_IF}" "$old_up" >/dev/null 2>&1
    [ -n "$up" ] && { ndc_ok ipfwd add "${XR_IF}" "$up" || ip rule add pref 18900 iif "${XR_IF}" lookup "$up"; }
    echo "$up" > "${RUN}/net.up_if"; log_info net "上游 ${old_up:-无} -> ${up:-无}"
  fi
  iptables -t nat -F XR_NAT
  for o in $outs; do iptables -t nat -A XR_NAT -s "${NET}" -o "$o" -j MASQUERADE; done   # outs=上游及其 464xlat 出口

  ex="$(expose_ifaces | tr '\n' ' ')"; echo "$ex" > "${RUN}/net.expose_if"
  . "${MODDIR}/scripts/vpnroutes.sh"; routes_sync      # 见 08；必须在 XR_FWD 重建前执行
  iptables -F XR_FWD
  iptables -A XR_FWD -o "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  iptables -A XR_FWD -i "${XR_IF}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  for o in $outs; do iptables -A XR_FWD -i "${XR_IF}" -o "$o" -j ACCEPT; done
  for i in $ex; do
    iptables -A XR_FWD -i "$i" -o "${XR_IF}" -d "${NET}" -j ACCEPT
    for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
      iptables -A XR_FWD -i "$i" -o "${XR_IF}" -d "$c" -j ACCEPT
    done
  done
  for o in $outs; do   # 未被 XR_EXPOSE=all 放行的上游：入站显式丢弃
    case " $ex " in *" $o "*) ;; *) iptables -A XR_FWD -i "$o" -o "${XR_IF}" -j DROP ;; esac
  done
  iptables -A XR_FWD -o "${XR_IF}" -j DROP
  iptables -A XR_FWD -i "${XR_IF}" -j DROP
  fwd_first
}
```

规则按标准路由器语义：手机当前上游（Wi-Fi、蜂窝或其他，不做区分）就是 VM 的 WAN，默认不信任。插 5G 卡、没有 Wi-Fi 时，蜂窝就是 VM 的 WAN。

1. 已建立的连接（回包，ESTABLISHED,RELATED）放行，排在所有 DROP 之前；
2. VM 访问上游（出站）放行：上游网卡及其 464xlat 出口 `v4-*`（纯 IPv6 蜂窝的 IPv4 实际从这里出去）；
3. `XR_EXPOSE` 列出的下游可以访问 VM 网段和 VPN 网段（hotspot：热点/USB 共享；all：再加非蜂窝上游所在局域网）；
4. 上游主动发往 xr0 的入站丢弃；
5. 其他进出 xr0 的流量一律丢弃。

手机侧只做过滤和源地址伪装：不使用 mangle 表，不改写 VM 发出的包（不限制 UDP/QUIC、不限目标端口、不改 fwmark/DSCP、不做 MSS 修改）；
MASQUERADE 只匹配源 `172.29.233.0/24` 且出口为上游的包。VM 里的 OpenVPN、Clash（OpenClash / nikki 等）看到的就是一条普通的 NAT 上行。

不在 XR_FWD 里的流量（例如热点设备正常上网）不受影响，规则执行完后自动 RETURN，交给 Android 原有的链处理。

## `net_watch`（每 10 秒一次）

```sh
net_watch() {
  while [ -d "/sys/class/net/${XR_IF}" ]; do
    sleep 10; load_config
    up="$(upstream_iface)"; ex="$(expose_ifaces | tr '\n' ' ')"
    if [ "$up" != "$(cat "${RUN}/net.up_if" 2>/dev/null)" ] || [ "$ex" != "$(cat "${RUN}/net.expose_if" 2>/dev/null)" ]; then sync_all; fi
    fwd_first
    iptables -t nat -C POSTROUTING -j XR_NAT 2>/dev/null || iptables -t nat -I POSTROUTING 1 -j XR_NAT
    ip rule | grep -q "^4800:.*to ${NET}" || ip rule add pref 4800 to "${NET}" lookup "$(lnt)"
    if [ "$(cat "${RUN}/net.mode")" = fallback ]; then
      ip route show table 97 | grep -q "^${NET} dev ${XR_IF}" || ip route replace "${NET}" dev "${XR_IF}" table 97
      routes_reassert      # 08：把 table 97 里的 VPN 手工网段路由补回
    fi
  done
}
```

`xr set XR_EXPOSE ...` 或 `xr set VPN_ROUTES ...` 之后会立即调用 `net.sh sync`（即 `sync_all`），不用等对账循环。

## `net_down`（逆序撤销，每条都容错）

```sh
net_down() {
  kill "$(cat "${RUN}/net_watch.pid" 2>/dev/null)" 2>/dev/null
  . "${MODDIR}/scripts/vpnroutes.sh"; routes_clear
  while iptables -D FORWARD -j XR_FWD 2>/dev/null; do :; done; iptables -F XR_FWD 2>/dev/null; iptables -X XR_FWD 2>/dev/null
  while iptables -t nat -D POSTROUTING -j XR_NAT 2>/dev/null; do :; done; iptables -t nat -F XR_NAT 2>/dev/null; iptables -t nat -X XR_NAT 2>/dev/null
  up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  [ -n "$up" ] && ndc ipfwd remove "${XR_IF}" "$up" >/dev/null 2>&1
  while ip rule del pref 18900 2>/dev/null; do :; done
  while ip rule del pref 4800 2>/dev/null; do :; done
  ndc ipfwd disable xroute >/dev/null 2>&1
  [ -f "${RUN}/ip_forward.orig" ] && cat "${RUN}/ip_forward.orig" > /proc/sys/net/ipv4/ip_forward
  ndc network route remove local "${XR_IF}" "${NET}" >/dev/null 2>&1
  ndc network interface remove local "${XR_IF}" >/dev/null 2>&1
  ip route flush table 97 dev "${XR_IF}" 2>/dev/null
  ip link del "${XR_IF}" 2>/dev/null
  rm -f "${RUN}"/net.* "${RUN}/routes.active" "${RUN}/ip_forward.orig" "${RUN}/net_watch.pid"
}
```

## 不做的事

- 不桥接，不做 DNAT/端口转发。
- 不改 Private DNS，不劫持 DNS，不改全局 sysctl（只改 xr0 自己的 `rp_filter` 和 `disable_ipv6`）。
- 不删除 Android 热点相关的 qdisc 和 eBPF 程序。

## 验收（在真机上）

- `net.sh up` 后：
  - `ip addr show xr0` 有 `172.29.233.1/24`；
  - `ip rule` 有 `4800: from all to 172.29.233.0/24 lookup local_network`（退回模式下是 `lookup 97`）；
  - `iptables -S FORWARD | sed -n 2p` 是 `-A FORWARD -j XR_FWD`；
  - `iptables -t nat -S XR_NAT` 有 MASQUERADE 规则。
- 手机执行 `ping 172.29.233.254` 能通；VM 里 `curl -I https://downloads.immortalwrt.org` 能通。
- 只开蜂窝（插 5G 卡、关 Wi-Fi）：VM 能上网；切换 Wi-Fi 和蜂窝 10 秒内，`net.up_if` / `net.up_out` 更新，VM 仍然能上网。
- 打开热点后 10 秒内，`net.expose_if` 包含热点网卡；热点设备能访问 `172.29.233.254`。设为 `XR_EXPOSE=off` 后访问不到。
- 用 `iptables -S XR_FWD` 检查：前两条是 ESTABLISHED,RELATED 放行，接着是 `-i xr0 -o <上游> -j ACCEPT`；有 `-i <上游> -o xr0 -j DROP`；最后两条是 `-o xr0 -j DROP`、`-i xr0 -j DROP`。
- `net.sh down` 后：xr0、XR_FWD、XR_NAT、pref 4800 和 18900 规则全部消失，`ip route show table 97` 里没有 xr0。
