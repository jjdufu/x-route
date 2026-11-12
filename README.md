# XRouter

在骁龙手机的 Gunyah 受保护虚拟机里运行 ImmortalWrt，为手机本机和热点提供网关、OpenVPN 组网与 OpenClash。SukiSU / KernelSU 模块，自带 WebUI。默认内存 768MB、系统盘 1024MB。镜像预装 OpenClash（默认关闭）和 mihomo 内核，在 LuCI 中自行启用。

- 热点交给 OpenWrt 分配地址，网关地址默认 `172.28.222.254/24`，可在设置里修改。上游 MTU 小于 1500 时，通过 DHCP / RA 把 MTU 下发给热点设备。
- OpenVPN 只需导入一个无密码的 `.ovpn`。所有隧道（tun*）的 MTU 和 MSS 会随上游 MTU 自动调整，组网页按隧道逐条显示。
- 内核内建 BBR 并设为默认拥塞控制。网卡开启 TSO / GSO / GRO，不启用软件流量卸载。根分区使用 noatime 并定期 fstrim。虚拟机时钟跟随手机。
- 日志统一导出为 zip。保活通知只显示在通知栏，不在锁屏显示。
- 组网页可直接启动或停止 OpenVPN。制品与界面文案将「虚拟机」改为「服务」。
- WebUI 采用赛博朋克视觉，首页用可点击芯片启停服务；启动过程以电路板由红转绿向外修复并流动电流表示，不再使用瀑布流日志。

## 安装

只支持全新安装：先在管理器里卸载旧版并重启，再刷入 `xrouter-sukisu-full-<版本>.zip`。需要支持 AVF pVM 的骁龙机型、SukiSU / KernelSU，以及至少 1.5GB 的 `/data` 剩余空间。

安装后默认不自启，在 WebUI 主页启动。LuCI 为 `http://172.29.233.254`，用户 `root`，空密码，请尽快设置。

## 命令

```sh
XR="sh /data/adb/modules/xrouter/scripts/xr"
$XR start | stop | status-json
```

## 构建

```sh
bash scripts/build-kernel.sh
bash scripts/build-rootfs.sh
bash scripts/package-module.sh
sh scripts/lint.sh
```

推送到 `main` 后由 CI 构建并发布正式版（标签 `<版本>-build.<序号>`）。
