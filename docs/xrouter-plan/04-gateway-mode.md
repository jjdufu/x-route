# 第 4 步：运行模式（全局 / 旁路）

## 目标

设置中的运行模式（本步先把逻辑与 CLI 做完，界面切换按钮与二次确认在第 7 步）：

- **全局**（默认）：XRouter 启动并就绪后，手机本机和热点的上网流量都先进入虚拟机，经 OpenWrt 处理，再从当时的 WAN（5G / 4G / Wi-Fi）出去。之后在 OpenWrt 里安装的 OpenClash 等，会自然处理这些流量。
- **旁路**：保持现有做法，不接管手机和热点上网。只有访问本机这台路由（`172.29.233.0/24`）或已同步的对端网段时，流量才进虚拟机。

切换必须先被确认后再生效（CLI 可直接 `xr set XR_MODE …`，WebUI 在第 7 步加弹窗）。VM 未就绪或已停止时，不得把手机自己的上网搞断。必须避免环路：虚拟机出网不能再被导回虚拟机。

WAN 仍是手机上游，默认不信任：放行出站和回包，丢弃主动入站。不要影响 OpenVPN。不要在手机侧用 fwmark、TPROXY、限端口、改 DSCP 等方式限制日后在 VM 内安装的 OpenClash（TUN、TPROXY、fullcone、策略路由）。

## 依赖

- 依赖第 1–3 步的路径、卸载与守护进程。
- 第 5 步的自动 VPN 路由会复用本步未破坏的 `routes_*` 与「iif xr0 走 WAN」规则。
- 客侧要配合做「单臂 NAT」（见下），可在本步于 `openwrt/files` 先改防火墙，第 6 步再并入主机名等固件改动；也可以本步只改手机侧、把客侧 NAT 放到第 5 或第 6 步，但第 8 步前必须两边齐全，否则全局模式在 QEMU/真机上都无法闭环。**本步必须同时改客侧单臂 NAT**，不要把环路修复留到后面。

## 要改的文件

- `module/config/config.conf`：新增 `XR_MODE="global"`；删除本方案已规定从配置消失的键（见索引），若希望分步更小，本步至少加上 `XR_MODE` 并实现转发；删键可与本步一起做完，避免键表长期不一致。
- `module/scripts/common.sh`：`KEYS`、`valid_kv`、`HASH_KEYS`（模式切换不必使 last_good 失效）
- `module/scripts/net.sh`：全局模式策略路由、防环路、未就绪时不接管
- `module/scripts/vm.sh` / 守护进程：就绪后再启用全局接管，停止或超时先撤掉接管
- `module/scripts/xr`：`after_set` 对 `XR_MODE` 立即 `net.sh sync`（仅当 VM 已就绪）
- `openwrt/files/etc/init.d/xrouter-net` 与防火墙：单臂 MASQUERADE
- 测试：`tests/test-net.sh`、`tests/test-common.sh`、`tests/test-xr.sh`，新增全局模式用例

## 关键设计（必须按此实现）

手机既是 VM 的 WAN 下一跳（`xr0` 为 `172.29.233.1`），又是「要经 VM 上网的客户端」。若手机应用以 `172.29.233.1` 为源把包送给 VM，VM 原样发回后，回程目的地址会是 `.1`，在手机上被本机接收，**回包进不了 VM**，OpenClash 也无法处理完整连接。

正确做法是单臂网关：

1. 手机 / 热点的上网包先送到 `172.29.233.254`，尽量保持原源地址（热点客户端保留其热点地址；本机应用使用 `172.29.233.1` 作为经 `xr0` 的源地址即可）。
2. **OpenWrt 对发往默认网关、目的不在 `172.29.233.0/24` 的流量做 MASQUERADE**，使离开 VM 再回到手机的包源地址是 `172.29.233.254`。
3. 手机把 `iif xr0` 且目的为公网的包按现有 WAN 逻辑转发并 MASQUERADE（源网段仍是 `172.29.233.0/24`）。
4. 回包解 NAT 后目的是 `.254`，被送回 VM；VM 再解 NAT 回原客户端（手机 `.1` 或热点地址）。

这样 VM 能看到完整双向流量。OpenVPN 的网点约定（`172.29.233.0/24` 出隧道不做 NAT）不受影响：单臂 MASQUERADE 只适用于「经 lan 网关去往非本网段」的流量，不得对 `tun+` 上的 `172.29.233.0/24` 源地址做 NAT。

### 手机侧策略路由（防环路）

约定（不要占用系统常用表号之外的混乱值，并写进注释）：

| 对象 | 值 | 作用 |
|---|---|---|
| 规则 pref 4800 | `to 172.29.233.0/24 lookup <local_network 或 97>` | 访问 VM 网段 |
| 规则 pref 4801 | `to <已同步对端网段> lookup …` | VPN 网段（第 5 步） |
| 规则 pref 18900 | `iif xr0 lookup <上游表>` | **VM 出网走真实 WAN，绝不再进 VM** |
| 表 233 + pref 19100 | 仅全局且 VM 已就绪时：`default via 172.29.233.254 dev xr0 src 172.29.233.1`，并复制当前上游与热点的直连网段路由 | 本机与热点的默认上网进 VM，局域网直连不绕 VM |

规则优先级必须保证 18900 优于 19100。`net_watch` 发现 netd 改写规则时，按上述顺序补回。

表 233 必须带上游 / 热点的直连路由，避免把「访问当前 Wi-Fi 局域网内其它设备」全部塞进 VM 造成环路或中断。默认路由才指向 VM。

### IPv6（与 IPv4 同构）

| 项 | 内容 |
|---|---|
| 地址 | `xr0` 为 `fd7a:7872:e9::1/64`，虚拟机 `eth0` 为 `::254`（ULA，固定，经 cmdline `xr.ip6` / `xr.gw6` 下发） |
| `ip -6 rule` 4800 / 18800 / 18900 / 19100 | 与 IPv4 相同语义；表 233 复制上游与热点的直连 / 前缀路由 |
| 虚拟机 IPv6 已开（客侧上报 `guest/ipv6=1`） | 表 233 `default via ::254 src ::1`；手机侧 `XR_NAT6` 对 `fd7a:7872:e9::/64` 做 MASQUERADE（NAT66）；虚拟机内单臂 NAT 伪装成 `::254` |
| 虚拟机 IPv6 已关 | 表 233 `unreachable default`；热点到上游的 IPv6 转发 `REJECT icmp6-adm-prohibited`。应用立即回落 IPv4，不会绕过虚拟机 |
| 内核无 `ip6tables -t nat` | 视为虚拟机 IPv6 不可用，按「已关」处理并记日志 |

开关状态只读虚拟机：固件首次开机把 `network.xr_eth0.ipv6` 设为 0（组件全部保留），`xrouter-vpn` 控制循环执行 `host/ipv6.cmd` 并把 `eth0` 的实际 `disable_ipv6` 写回 `guest/ipv6`。`xr ipv6 on|off` 写命令、等客侧确认后 `net.sh sync`，不重启虚拟机。旁路模式不改本机 IPv6，只放行虚拟机自身 IPv6 出网。

### 何时启用 / 撤销全局接管

仅当同时满足：`XR_MODE=global`、`vm_running`、已看到 `XR_READY`（state 为 `ready` 或 `stable`）时，才安装 pref 19100 与表 233。

出现以下任一情况，立即撤销 19100 与表 233，让本机恢复 Android 自己的默认路由：

- `xr stop` / 守护进程停止 VM
- 就绪超时，熔断停机
- crosvm 意外退出（`net_watch` 或守护进程发现 xr0 或 pid 消失）
- 切换到旁路（确认后）

切到全局但 VM 尚未就绪：只保存配置，不安装接管规则。就绪回调里再 `sync`。

### iptables

- 旁路：保持现有 XR_FWD / XR_NAT 语义（暴露面、WAN 不信任、NAT 仅源 `172.29.233.0/24` 出上游）。
- 全局：在此基础上增加
  - 允许热点接口 `iif <tether> -o xr0` 的转发（目的不限于 VM 网段）；
  - 允许 `iif xr0 -o <tether>` 回给热点客户端（RELATED,ESTABLISHED 应覆盖，若不够再补 ACCEPT）；
  - **仍然** `iif xr0 -o <wan> ACCEPT`，WAN 入站未放行的丢弃；
  - **不要** 在手机侧对 VM 的包做 mangle、TPROXY、端口限制、MARK、DSCP。
- NAT 仍只对源属 `172.29.233.0/24`、出上游口 MASQUERADE。不要把热点源地址在进 VM 之前 SNAT 掉（除非后续发现某机型无此无法转发；若做必须写入第 8 步偏差）。

### 客侧单臂 NAT

在 `xrouter-net` 或 uci-defaults / fw4 include 中增加：出 `eth0`、目的不是 `172.29.233.0/24` 的流量 MASQUERADE。不要对 `tun+` 上本网段做 NAT。不要为此关闭 nft tproxy / fullcone / 策略路由能力。现有「默认 fullcone=0、flow offloading=0」可保持，由用户或插件自行打开（QEMU 已覆盖可开启）。

### 配置键清理（建议在本步完成）

删除并在代码中写死：`VM_SWIOTLB_MB=64`、`READY_TIMEOUT_SEC=180`、`STABLE_SEC=180`、`VM_NET=1`、`VM_DISK_RW=1`、`AUTOSTART_DELAY_SEC=60`。更新 `valid_kv`、`check-config-keys.sh` 与测试。`VPN_MODE` / `VPN_ROUTES` 若本步仍保留，第 5 步必须删除；推荐本步先留着以免与第 5 步冲突，但 `XR_MODE` 必须落地。

`xr set XR_MODE` 只接受 `global` 或 `bypass`。

## 验收标准

- `tests/test-net.sh` 增加：
  - 旁路：行为与现网测试等价（默认路由不指向 `.254`，无 pref 19100）。
  - 全局且模拟「已就绪」：出现 pref 19100 与表 233 的 default via `.254`，且存在 `iif xr0` 走上游的规则；热点到 xr0 的转发允许不只目的为 VM 网段。
  - 全局但未就绪 / 调用 down：没有 19100，本机默认不被改写。
  - 仍无对 VM 出站包的 mangle / 端口限制；WAN 入站默认 DROP。
- `valid_kv XR_MODE` 只接受两个取值，默认配置为 `global`。
- 客侧文件中有单臂 MASQUERADE，且明确排除本网段与对 VPN 本网段不 NAT 的注释或条件。
- `sh scripts/lint.sh` 通过。
