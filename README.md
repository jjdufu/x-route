# XRouter

在骁龙手机的 Gunyah 受保护虚拟机里运行 ImmortalWrt，为手机本机和热点提供网关、OpenVPN 组网与 OpenClash。SukiSU / KernelSU 模块，自带 WebUI。默认内存 768MB、系统盘 1024MB。镜像预装 `luci-theme-argon` 默认 LuCI 主题（默认关闭登录页背景图与毛玻璃模糊）、OpenClash（默认关闭，含 zashboard）与 mihomo 内核，可在 LuCI 或模块 WebUI 中管理。

- 热点交给 OpenWrt 分配地址，网关地址默认 `172.28.222.254/24`，可在设置里修改。上游 MTU 小于 1500 时，通过 DHCP / RA 把 MTU 下发给热点设备。
- OpenVPN 支持导入多份无密码的 `.ovpn` / `.conf` 配置，导入后直接在网络页「异地组网」按卡片列出，可对每份配置单独启动、停止或删除；所有隧道（tun*）的 MTU 和 MSS 随上游 MTU 自动调整。
- 内核内建 BBR 并设为默认拥塞控制。网卡开启 TSO / GSO / GRO，不启用软件流量卸载。根分区使用 noatime 并定期 fstrim。虚拟机时钟跟随手机。
- 日志统一导出为 zip。保活通知只显示在通知栏，不在锁屏显示。制品与界面文案将「虚拟机」改为「服务」。
- WebUI 采用 Google Material Design 3（M3 Expressive）规范与官方组件，支持跟随系统及深浅色切换、默认三栏底栏导航（状态 · 网络 · 系统）、首页色阶状态主卡（含三阶段启动步骤条与波浪进度指示器）、状态页累计流量汇总与按需分源详情弹窗、系统页可折叠关于（含立即卸载与「分流管理」开关）及触感返回反馈；当 OpenClash 核心运行且存在 zashboard 并启用「分流管理」时，底部在「网络」与「系统」之间动态显示「分流」页，经由纯内部 `hvc1` 通道无 IP 直连内嵌 zashboard。
- 状态页一屏总览：实时上传和下载速率（由手机侧 iptables 计数在内存中计算，不落盘）、OpenWrt CPU、内存和负载、在线设备数（本机加热点，按邻居表统计，设备离开后会延迟几分钟才减少）、累计流量。
- 配置备份：在系统页「配置备份」中导出或导入明文 zip，内容包括手机侧设置、VPN 配置和 OpenWrt 的 sysupgrade 备份。导入后服务会自动重启。服务必须在运行，并且需要新系统盘。**备份文件包含密码和密钥，请妥善保管。**
- 流量快照：变化达到 64 MiB 或满 30 分钟时才写入闪存；正常停止和关机时会立即写入。意外掉电最多丢失这一区间内的计数。
- 日志和导出文件名使用手机本地时区。
- VPN 推送的网段如果与当前 Wi-Fi 同网段，会显示「本地直连」，表示不经过 VPN；切换网络后自动恢复。

## 安装

只支持全新安装：先在管理器里卸载旧版并重启，再刷入 `xrouter-sukisu-full-<版本>.zip`。需要支持 AVF pVM 的骁龙机型、SukiSU / KernelSU，以及至少 1.5GB 的 `/data` 剩余空间。

安装后默认不自启，在 WebUI 主页启动。LuCI 为 `http://172.29.233.254`，用户 `root`，空密码，请尽快设置。

## 命令

```sh
XR="sh /data/adb/modules/xrouter/scripts/xr"
$XR start | stop | status-json
$XR config-export                    # 导出到 Download/xrouter-config-<时间>.zip
$XR config-import <zip> [--restart]  # 导入；--restart 立即重启服务
$XR guest-stats                      # 调试：读取 OpenWrt 负载与设备
```

## 构建

```sh
bash scripts/build-kernel.sh
bash scripts/build-rootfs.sh
bash scripts/package-module.sh
sh scripts/lint.sh
```

Rust 工具链由根目录 `rust-toolchain.toml` 锁定为 1.100.0（edition 2024），首次构建时 rustup 会自动安装。

Rust 改动以 `bash scripts/gate.sh` 全绿为准，标准见 [`docs/门禁-Rust最严Lint.md`](docs/门禁-Rust最严Lint.md)。

推送到 `main` 后由 CI 构建并发布正式版（标签 `<版本>-build.<序号>`）。
