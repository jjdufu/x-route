# XRouter 真机测试清单

只在用户的测试手机上，并且得到用户同意后执行。每一项都记录：结果（通过 / 失败）、相关日志片段（`xr log xr|console|ctl|vmm`）、发现的问题。不记录 IMEI、序列号、证书和密码。

## 0 前置检查（在安装之前，只读）

| 检查 | 命令 | 用途 |
|---|---|---|
| crosvm 参数 | `/apex/com.android.virt/bin/crosvm run --help 2>&1` | 确认 `--no-balloon`、`--swiotlb`、`--serial ...hardware=virtio-console`、`--shared-dir` 都存在，以及 `--shared-dir` 是否支持 `uidmap`/`gidmap`/`ugid_map`。根据结果填写 `common.sh` 里的 `RW_MAP_OPT` |
| ndc | `ndc network interface add local lo 2>&1; ndc ipfwd 2>&1` | 确认 ndc 可用、子命令格式；前者应返回错误码（lo 不允许加入），能证明命令能被解析即可 |
| netd 规则格式 | `ip rule` | 存在 `fwmark 0x0/0xffff iif lo lookup <iface>`，`upstream_iface` 依赖它 |
| busybox | `/data/adb/ksu/bin/busybox unshare --help; ... mount --help` | 确认 ro 共享用到的命令存在 |
| 地址冲突 | `ip -4 addr; ip route show table all \| grep 172.29.` | `172.29.233.0/24` 没有被占用 |

## 1 安装与启动

1. 卸载旧版并重启。确认 `/data/adb/xrouter` 已经不存在。
2. 刷入 `xrouter-sukisu-full-<版本>.zip`：安装日志显示空间检查通过；`images/` 下有 `Image`、`openwrt-rootfs.img`（大小为 `VM_DISK_SIZE_MB`）、`openwrt-rootfs.img.gz`；模块目录中没有 `data/`。
3. 重启手机：VM **没有**自动启动（`AUTOSTART=0`），`module.log` 有记录 “AUTOSTART=0”。
4. 在 WebUI 点概览页的电源按钮：
   - 顶部标题栏固定，四个 Tab 为：概览、组网、日志、设置；点击或左右滑动切换时标题不动；
   - 启动中电源按钮外圈转动，依次显示「启动虚拟机 → 网络就绪 → 运行中」；`READY_TIMEOUT_SEC` 内出现 `XR_READY`，记录走的是 hvc 还是 ttyS0；
   - 启动完成后按钮呈绿色呼吸光，标题栏状态为「运行中」（不要出现「已稳定」）；后台在 `STABLE_SEC` 后写入 `last_good`；
   - 启动失败时按钮变红并显示原因，状态为「异常」；
   - 「存储共享 → 目录」与「导入配置」弹出深色文件夹浏览，从「内部存储」开始，路径可点击跳转。
5. 连续启动、停止 VM 10 次：整机不重启，每次停止后 `ip link show xr0` 都不存在，`ip rule` 恢复原样。

## 2 稳定性与熔断

1. 运行 1 小时，期间做以下操作：熄屏、亮屏，Wi-Fi 和蜂窝网络切换，打开 / 关闭热点。记录是否出现整机复位。
2. 如果出现复位：开机后 `tripped` 存在，`AUTOSTART=0`，WebUI 显示「整机复位后自启已关闭」，电源按钮不可用。点“确认”后可以再次启动。
3. 出现复位时按以下顺序二分，每次只改一项，每项启动 3 次：
   1. `VM_NET=0`；
   2. `VM_DISK_RW=0`；
   3. `VM_SWIOTLB_MB=32` 和 `128`；
   4. `VM_MEM_MB=384`；
   5. `VM_CPUS=1`。
   
   每次记录 `vmm.log` 的最后 50 行。
4. 自启：手动成功启动后打开 AUTOSTART，重启手机，经过延迟后（并且已解锁）VM 自动启动。修改 `VM_MEM_MB` 后再重启手机：不会自启，`module.log` 说明原因是 hash 不一致。

## 3 VM 系统

- `uname -r` 为 `6.12.103`；`apk update && apk add kmod-wireguard`（或其他 kmod）能安装并 `modprobe` 成功（验证 vermagic 兼容）。
- `apk list -I | grep -E "openvpn|luci-app-openvpn|tcpdump|ttyd"` 都存在，`htop` 不存在。
- 执行 `xr expand 2048` 后再启动，`df -h /` 约为 2GB；执行 `xr reset-disk` 后恢复默认。
- 首次开机 `grep root /etc/shadow` 为 `root::`（与上游一致）；空密码可登录 LuCI、ttyd 和 ssh。在 LuCI 设置密码后重启 VM，新密码仍然有效。

## 4 网络

| 场景 | 期望 |
|---|---|
| 手机访问 `http://172.29.233.254` | 能打开 LuCI |
| VM 内 `ping -c3 223.5.5.5`、`nslookup example.com` | 走 Wi-Fi 时成功 |
| 只开蜂窝网络（插 5G 卡、关 Wi-Fi） | VM 能上网（蜂窝作为 WAN，`net.up_if` 为 rmnet_data*）；从蜂窝侧主动访问 VM 不通 |
| Wi-Fi 与蜂窝切换 | 10 秒内 `net.up_if` 更新，转发规则跟着切换 |
| 热点客户端，管理访问「内网」 | 能访问 `.254`；「本机」时访问不了，切换不需要重启 VM |
| 管理访问「所有」，上级路由器配置静态路由 | 局域网其他设备能访问 `.254`（这一项需用户同意后在路由器上临时配置，测完删除） |
| 关闭热点后再打开 | 规则自动恢复 |
| 停止 VM | XR_FWD、XR_NAT、规则 4800/4801/18800/18900/19100、table 97/233 全部清空；`ndc ipfwd` 恢复；`ip_forward` 恢复原值 |
| 运行模式「全局」，虚拟机已就绪 | 手机和热点访问公网经 `.254`；虚拟机自己访问公网不回灌（`iif xr0` 仍走上游） |
| 运行模式「旁路」，或虚拟机未就绪 / 已停止 | 手机和热点自行出网，不被导入虚拟机 |
| 全局，设置中 IPv6 关（默认） | 手机与热点访问纯 IPv6 站点立即失败并回落 IPv4（`ip -6 route get 2400:3200::1` 为 unreachable）；不经上游直出 |
| 全局，设置中打开 IPv6 | 不重启虚拟机；手机与热点 `curl -6 https://ipv6.icanhazip.com` 成功，出口经虚拟机；LuCI 设备 eth0「启用 IPv6」同步为开 |
| 停止 VM | `ip -6 rule` 4800/18800/18900/19100、`ip -6` 表 233、XR_FWD6、XR_NAT6 全部清空 |

## 5 共享

按 docs/v2-plan/09-share.md 的验收逐项执行：关闭、ro、rw（仅在 `RW_MAP_OPT` 非空时）、未解锁时启动、大文件读取。
路径填写 `/sdcard` 或 `/storage/emulated/0` 时，应挂载 `/data/media/0`。

## 6 OpenVPN

1. 在组网页「导入配置」选择 `.ovpn`，或在 LuCI 导入并启用：不重启虚拟机；`tun0` 获得地址，`logread` 中没有报错。两种导入方式连上后，服务器 push 的网段都应出现在「路由网段」，并自动同步到手机。
2. 记录 DCO 状态（QEMU 中 openvpn 显示 `[DCO]` 且官方 `ovpn` 模块可加载，真机上以实际日志为准），以及 DNS 推送的行为。需要时按 docs/v2-plan/08-openvpn.md 设置 `dns-updown disable`。
3. 连上后：
   - 手机 `ip route get <对端地址>` 经过 xr0；
   - 手机能访问对端服务；
   - 默认路由 / redirect-gateway 不同步到手机。
4. 与 VM 网段重叠或未对齐的网段进入 `routes_rejected`，不下发。
5. 对端配置好 iroute 后：对端能访问手机侧的 `172.29.233.0/24`；热点网段出隧道仍做 NAT。
6. MTU：`ping -M do -s 1300 <对端>` 成功；大文件下载没有卡住。
7. 断开重连：关闭 Wi-Fi 30 秒后再打开，OpenVPN 自动重连，手机路由仍然有效。

## 7 卸载

在管理器卸载模块并重启：进程退出；`/data/adb/xrouter`（含日志和系统盘）被删除；`ip rule`、iptables、`ndc` 网络中没有本模块残留（含 19100 / table 98）；通知和 `xrouter` 唤醒锁清除。Download 里此前导出的日志包可以保留。

## 建议的首测顺序

1. 第 0 节前置检查（只读）。
2. 第 1 节安装与第一次手动启动：重点看是否整机复位、`XR_READY` 走的通道。
3. 第 4 节手机访问 LuCI、VM 上网、停止后规则清空。
4. 第 2 节 1 小时稳定性，然后再开自启。
5. 其余各节。

## 附：OpenVPN / Clash 能力核对（VM 内，可离线）

把仓库里的 `tests/vm-caps.sh` 拷进 VM 执行（`ssh root@172.29.233.254 sh -s < tests/vm-caps.sh`），最后一行应为 `CAPS_OK`。
它核对：预装官方 kmod 可 modprobe（tun、nft_tproxy、nft_socket、nft_fullcone、inet_diag、ovpn 等）；
socket / tproxy / redirect / masquerade / fullcone 等 nft 规则能加载；fwmark 策略路由、TUN、IPv6 转发可用；
fw4 的 fullcone 与 flow offloading 可以开启；内核日志无模块兼容错误。

插 5G 卡、关闭 Wi-Fi 时另外确认：VM 内 `ping 223.5.5.5`、`curl -I https://downloads.immortalwrt.org` 成功；
手机 `iptables -S XR_FWD` 中有 `-i xr0 -o rmnet_dataX -j ACCEPT` 与 `-i rmnet_dataX -o xr0 -j DROP`，
纯 IPv6 运营商（464xlat）时还应出现 `v4-rmnet_dataX` 的同类规则和 MASQUERADE。


## 附：本机已跑过的检查（发版前）

环境：Grok Bot box，Asia/Shanghai。下列结果不得用来代替真机测试。

| 检查 | 结果 |
|---|---|
| `sh scripts/lint.sh`（shellcheck、`sh -n`、全部 `tests/test-*.sh` 在 dash / mksh / busybox sh 下） | 通过 |
| `sh scripts/check-config-keys.sh` | 通过（19 个配置键） |
| `bash scripts/package-module.sh --lite-only` | 生成 `xrouter-sukisu-lite-v3.0.0.zip`：`id=xrouter`，无旧路径，无 META-INF |
| `tests/test-webui.sh`（node） | 通过 |
| `tests/e2e-qemu.sh` | 通过（先用当前源码重新构建 rootfs，再启动；已核对主机名 XRouter、型号、zh_cn、fw4、OpenVPN、透明代理能力） |

