# 08 OpenVPN 互通

目标：VM 用 rtvpn 签发的 `.ovpn` 连接 VPN；手机本机和热点设备通过 VM 访问远端内网。VPN 网段**只能在 WebUI 里手工填写**（`VPN_ROUTES`），VM 不往手机上报任何路由。

## VM 侧（固件里已预置，见 04）

- 软件包：`openvpn-openssl` 2.7.8 及其依赖、`luci-app-openvpn`、`luci-i18n-openvpn-zh-cn`、`openssl-util`。
- `network.vpn`：`proto none`，`device tun0`。
- 防火墙 vpn 分区：`device tun+`，`input REJECT`，`output ACCEPT`，`forward REJECT`，`masq 1`，`mtu_fix 1`；转发 lan→vpn。
- `x-route-net` 每次开机按 cmdline 里的 `xr.vpnmode` 调整防火墙：

| `VPN_MODE` | vpn 分区 | 转发 | rtvpn 侧怎么配 |
|---|---|---|---|
| `terminal`（默认） | `masq 1`，所有流量伪装成 VM 的隧道地址 | 只有 lan→vpn | 普通客户端，不宣告网段 |
| `site` | `masq 1` + `masq_src '!<前缀>.0/24'`：只伪装热点设备等外部源地址 | lan→vpn 和 vpn→lan | 网点网关，宣告 `<XR_NET_PREFIX>.0/24`；多台设备时各自的前缀不能相同 |

改 `VPN_MODE` 后必须重启 VM 才生效（cmdline 变了）。WebUI 保存后提示重启。

## 导入 `.ovpn`（用户在 LuCI 操作，x-route 不提供文件通道）

1. 在 rtvpn 里**给 VM 单独签一个客户端**（不能和其他设备共用一个 CN），下载 `.ovpn`：内嵌单文件，或 p12 版本加 `.p12` 文件。
2. 打开 LuCI → VPN → OpenVPN → 添加实例，名称填 `rtvpn`，上传 `.ovpn`。p12 版本另外把 `.p12` 上传到 `/etc/openvpn/`，路径要和 `.ovpn` 里 `pkcs12` 一行写的一致。
3. 勾选启用，保存并应用。生成的 uci 是 `config openvpn 'rtvpn'`，其中 `option config '/etc/openvpn/rtvpn.ovpn'`，`option enabled '1'`。
4. rtvpn 服务端主动断开后，客户端靠配置里的 `resolv-retry infinite` 和 keepalive 自动重连；进程退出时由 procd 重新拉起。

DCO：内核没有 `ovpn` 模块，`kmod-ovpn-backports` 也加载不了，openvpn 2.7 会在日志里提示后自动改用 tun。不另外做配置。

## 手机侧：手工 VPN 网段 → `module/scripts/vpnroutes.sh`（新增）

被 `net.sh` 引入（`. vpnroutes.sh`），使用 `common.sh` 里的 CIDR 工具函数。

### 校验 `routes_valid`

输入是 `VPN_ROUTES`，逗号分隔。输出是合法的 CIDR 列表，每行一个；不合法的写进日志并丢弃。

```sh
routes_valid() {
  up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  busy="${XR_NET_PREFIX}.0/24 127.0.0.0/8 169.254.0.0/16"
  for i in $up $(tether_ifaces); do
    busy="$busy $(ip -4 -o addr show dev "$i" | awk '{print $4}')"
  done
  n=0
  for c in $(echo "${VPN_ROUTES}" | tr ',' ' '); do
    valid_cidr "$c"   || { log_err vpn "格式错误 $c"; continue; }
    cidr_private "$c" || { log_err vpn "不是私网地址 $c"; continue; }
    net="$(ip2int "${c%/*}")"; p="${c#*/}"; m=$(( (0xFFFFFFFF << (32-p)) & 0xFFFFFFFF ))
    [ $(( net & m )) -eq "$net" ] || { log_err vpn "主机位不为 0：$c"; continue; }
    ok=1; for b in $busy; do cidr_overlap "$c" "$b" && { log_err vpn "$c 与 $b 重叠"; ok=0; break; }; done
    [ $ok = 1 ] || continue
    n=$((n+1)); [ $n -le 64 ] || { log_err vpn "超过 64 条，其余忽略"; break; }
    echo "$c"
  done | sort -u
}
```

WebUI 前端用同样的规则先校验一遍，后端 `valid_kv VPN_ROUTES` 也会再校验格式（见 05）。

### 生效 `routes_sync`、补回 `routes_reassert`、清除 `routes_clear`

```sh
route_add() {
  if [ "$(cat "${RUN}/net.mode")" = ndc ]; then ndc network route add local xr0 "$1" "${XR_NET_PREFIX}.254" >/dev/null 2>&1
  else ip route replace "$1" via "${XR_NET_PREFIX}.254" dev xr0 table 97; fi
  ip rule del pref 4801 to "$1" 2>/dev/null; ip rule add pref 4801 to "$1" lookup "$(lnt)"
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
routes_reassert() { for c in $(cat "${RUN}/routes.active" 2>/dev/null); do
  ip route show table 97 | grep -q "^$c via" || route_add "$c"; done; }
routes_clear() { for c in $(cat "${RUN}/routes.active" 2>/dev/null); do route_del "$c"; done; rm -f "${RUN}/routes.active"; }
```

- `routes_sync` 在 `sync_all` 里、XR_FWD 重建之前调用，因为 XR_FWD 里的 VPN 网段放行规则读取的是 `routes.active`。
- 用户在 WebUI 修改 `VPN_ROUTES` 后，`xr set` 会调用 `net.sh sync`，立即生效。
- 规则 pref 4801 专门给 VPN 网段用，和 4800（VM 网段）分开，方便清理。

## 数据流

- 热点设备 → 远端内网：
  - 设备默认网关是手机，手机按 pref 4801 规则查 local_network（或 table 97），下一跳 `172.29.233.254`；
  - 经 XR_FWD 放行进入 xr0，到达 VM；
  - VM 经 lan→vpn 转发进入 tun0，在 vpn 分区做伪装（terminal 模式全部伪装；site 模式只伪装非本网段的源地址）。
- 手机本机 App → 远端内网：同一条路由，源地址是 `172.29.233.1`。site 模式下内网看到真实地址，可以反向访问手机。
- 回程：VM 默认网关是 `172.29.233.1`，经 XR_FWD 的已建立连接规则回到热点设备。

## MTU 与 DNS

- vpn 分区开了 `mtu_fix`，TCP MSS 会被修正。实测 `ping -M do -s 1372` 不通时，在 LuCI 里给 `rtvpn` 实例加 `mssfix 1360`。
- DNS：rtvpn 默认推送 DNS。连接前后对比 VM 的 `/tmp/resolv.conf` 和 `/tmp/resolv.conf.d/resolv.conf.auto`：
  - 内容被改了、并且影响 VM 上网：在实例里加 `dns-updown disable`（OpenVPN 2.7 的选项），或者在 rtvpn 里对这个客户端不推送 DNS；
  - 没被改：不处理。结论记到 13。
- 热点设备的 DNS 仍然走 Android 本身。访问内网时用 IP，或者在设备上手工把 DNS 设成内网 DNS。

## 验收（在真机上）

- LuCI 导入内嵌版和 p12 版各成功一次：VM 里 `ip addr show tun0` 有隧道地址；`logread | grep openvpn` 出现 `Initialization Sequence Completed`。
- 在 rtvpn 里踢掉这个客户端：60 秒内自动重连。
- WebUI 填一个内网网段并保存：手机 `ip rule` 出现 `4801: ... to <CIDR> lookup local_network`，`ip route show table local_network`（或 `97`）出现该网段 via `172.29.233.254`，XR_FWD 里出现对应的放行规则。
- 手机本机和热点笔记本 `ping` 内网某台主机都能通。在 rtvpn 侧抓包看源地址：terminal 模式是 VM 的隧道地址；site 模式下手机是 `172.29.233.1`，热点设备是 VM 隧道地址。
- 填一个公网网段或者和 `172.29.233.0/24` 重叠的网段：被拒绝，`xr.log` 有记录，`routes.active` 里没有它。
- 清空 `VPN_ROUTES` 或停止 VM：pref 4801 规则和对应路由全部消失。
