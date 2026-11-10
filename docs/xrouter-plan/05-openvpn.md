# 第 5 步：OpenVPN 互联

## 目标

- 可在模块里上传并启用 ovpn：由模块写入 OpenWrt，勾选启用并拨号。不重启虚拟机，最多重载 OpenVPN 进程。
- 隧道起来、拿到地址和服务器 **push 的实际路由** 后，自动同步到手机。不从 ovpn 文件解析网段。在 LuCI 里导入并启用同样自动同步。
- 去掉 `VPN_MODE` 与手工 `VPN_ROUTES`。固定按网点方式：
  - 本网段 `172.29.233.0/24` 出隧道不做 NAT，对端可回访；
  - 热点网段出隧道仍做 NAT（网段会变，无法在对端固定宣告）；
  - vpn 区 `input` 拒绝（需要时再在 LuCI 放行具体端口），`forward` / `output` 允许，并开启 `lan ↔ vpn` 双向转发。
- 界面相关的上传、状态、已接入网段展示在第 7 步完成；本步把 CLI、客侧代理、同步与防火墙做完。

## 依赖

- 依赖第 3 步：VM 已在运行、控制通道或共享目录可用。
- 依赖第 4 步：手机侧路由同步不得破坏 `iif xr0` 走 WAN；全局模式下推送的默认路由不要再叠一套劫持。
- 固件 overlay 在本步改防火墙与代理；第 6 步构建 rootfs 时带上这些文件。

## 要改的文件

- 新增客侧：`openwrt/files/etc/init.d/xrouter-vpn`（或 `xrouter-agent`），以及 OpenVPN up/down 脚本，例如 `openwrt/files/etc/openvpn/xrouter-up.sh`、`xrouter-down.sh`
- 调整 `openwrt/files/etc/uci-defaults/10-xrouter`、`openwrt/files/etc/init.d/xrouter-net`：去掉 `xr.vpnmode` 分支，永远按网点防火墙生效
- 新增或改写 `module/scripts/vpnroutes.sh`：数据源改为「客侧上报的实际路由」，删除手工列表逻辑
- `module/scripts/vm.sh`：为控制通道增加固定的 virtiofs 共享（见下）
- `module/scripts/xr`：增加导入 / 查询命令；`status-json` 改为连接状态与已同步网段
- `module/config/config.conf`、`common.sh`：删除 `VPN_MODE`、`VPN_ROUTES`
- `module/scripts/net.sh`：`routes_sync` 改读运行时文件
- 测试：`tests/test-net.sh`、`tests/test-xr.sh`、`tests/test-common.sh`，新增 VPN 导入与同步用例；`tests/e2e-qemu.sh` 去掉对 `VPN_MODE=terminal` 时 `no_vpn_lan` 的断言，改为永远存在 lan↔vpn 转发

## 具体做法

### 1. 主机与客机的控制通道

增加**始终启用**的控制目录 virtiofs（与可选的手机存储共享分开）：

- 主机：`${RUN}/guest/`（模块创建，权限 0700）
- crosvm：`--shared-dir ${RUN}/guest:xrctl:type=fs:cache=auto:timeout=1`
- 客机：`/var/run/xrouter`（`xrouter-vpn` 在开机时 `mount -t virtiofs xrctl /var/run/xrouter`）

约定文件（均明文、无密钥落盘到模块日志）：

| 路径 | 方向 | 含义 |
|---|---|---|
| `host/openvpn.ovpn` | 主机→客机 | 待启用的配置 |
| `host/openvpn.cmd` | 主机→客机 | `enable` / `disable` / `reload` |
| `guest/openvpn.status` | 客机→主机 | `down` / `connecting` / `up` |
| `guest/openvpn.addr` | 客机→主机 | 隧道地址（若有） |
| `guest/openvpn.routes` | 客机→主机 | 实际生效的路由，一行一个 CIDR |
| `guest/openvpn.err` | 客机→主机 | 简短失败原因（不要写入证书内容） |

若 virtiofs 控制目录挂载失败：客机把状态行写到 hvc0（`XR_VPN_STATUS …`、`XR_VPN_ROUTES …`），主机从 `vm-ctl.log` 解析并写入同样的运行时文件。导入配置的回退：仅接受来自 `172.29.233.1` 的客机临时 HTTP（例如 `127` 侧监听在 `172.29.233.254:9270`），用于把 ovpn 写入 `/etc/openvpn/`。不要依赖在模块里捆绑 ssh 客户端作为主路径。

控制目录只用于模块与 VM 之间，不共享到 `/sdcard`。

### 2. 上传并启用

CLI（WebUI 第 7 步调用）：

- `xr vpn-import`：从 stdin 或已放在 `${RUN}/incoming.ovpn` 的文件安装。因 `ksu.exec` 不便传 stdin，提供分片写入：
  - `xr vpn-stage-begin`
  - `xr vpn-stage-append <base64>`（每片长度限制在命令行安全范围内，如 2KB）
  - `xr vpn-stage-commit`：解码校验为文本配置后写入 `host/openvpn.ovpn`，写入 `host/openvpn.cmd=enable`
- 客机代理发现 cmd 后：把配置安装到 `/etc/openvpn/`（UCI 启用对应 instance，或官方 `openvpn.sample` 方式），`/etc/init.d/openvpn reload` 或 restart **仅 OpenVPN 进程**，禁止重启网络栈或整机。
- VM 未运行时：配置暂存在主机 `${XR_DATA}/config/openvpn.ovpn`，待 `vm_start` 后写入控制目录并 enable。不做旧版兼容，只是「先导入后启动」的队列。
- 不要在日志中打印证书、私钥、`auth-user-pass` 密码。测试用配置必须是无秘密的占位内容。

### 3. 路由同步（不解析 ovpn）

客机在 OpenVPN `up` / `route-up`（以及定时核对）时：

- 读取实际路由：`ip -4 route show dev tun0`（及 redirect-gateway 可能添加的 `0.0.0.0/1`、`128.0.0.0/1` 等）。
- 写入 `guest/openvpn.routes`，状态设为 `up`。
- `down` 时清空 routes、状态 `down`。

主机 `net_watch`（或独立间隔更短的 vpn watch，建议 2–5 秒，随 net down 退出）：

- 读取 `guest/openvpn.routes`（或 hvc 回退结果），调用原 `route_add` / `route_del` 与 `routes.active` 对账。
- **不要**再读取配置里的 `VPN_ROUTES`。
- 过滤：丢弃与 VM 网段、当前上游、热点网段重叠的项；格式非法的丢弃并记日志。
- `0.0.0.0/0`、`0.0.0.0/1`、`128.0.0.0/1` **不同步到手机**（全量默认路由在「全局」模式下已由第 4 步处理；在旁路模式下也不要因为一次 push 就把旁路变成全局）。「已接入的网段」只展示对其它已同步的网段。
- 私网与非默认的推送路由都应同步（不限于手工时代的私网限制，但必须做重叠检查）。若推送了公网业务网段，允许下发；若与当前上游重叠则拒绝。
- LuCI 导入的配置只要建立起 `tun0` 并出现路由，走同一代理，无需模块再操作。

### 4. 防火墙（固定网点）

uci-defaults / 每次开机的 `xrouter-net`：

- 删除 `xr.vpnmode` 内核参数及 terminal/site 分支。
- `firewall.vpn`：`input=REJECT`，`output=ACCEPT`，`forward=ACCEPT`（或等价允许转发），`masq=1`，`mtu_fix=1`。
- `masq_src` 排除 `!172.29.233.0/24`（即本网段源地址出 vpn 不伪装）。
- `firewall.lan_vpn` 与 `firewall.vpn_lan` 双向转发始终创建。
- 单臂 NAT（第 4 步）不得覆盖上述 vpn 不伪装规则。

`vm.sh` 的 cmdline 不再传 `xr.vpnmode`。

### 5. 状态

`status-json` 的 `vpn` 对象改为例如：

```json
"vpn":{"status":"up","addr":"…","routes_active":["10.0.0.0/16"],"routes_rejected":["…"]}
```

不要再输出 `mode`。无秘密字段。

## 验收标准

- 单元测试：写入模拟 `guest/openvpn.routes` 后 `net.sh sync` 只下发合法、不重叠、非默认路由；清空文件后路由撤销。
- 单元测试：`vpn-stage-*` 能把占位配置拼起来写入控制目录并设置 `enable`；不把私钥写进 `module.log`。
- `VPN_MODE` / `VPN_ROUTES` 从配置、KEYS、valid_kv、WebUI（若尚未重做则先去掉相关控件以免失效键）中消失。
- 客侧默认即有 `vpn_lan` / `lan_vpn`，不再依据 cmdline 删除。
- `sh scripts/lint.sh` 通过。
