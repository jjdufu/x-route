# XRouter

在骁龙手机的 Gunyah 受保护虚拟机里运行 ImmortalWrt，为手机本机和热点提供网关、OpenVPN 组网与 OpenClash。SukiSU / KernelSU 模块，自带 WebUI。默认内存 768MB、系统盘 1024MB。镜像预装 OpenClash（默认关闭）和 mihomo 内核，在 LuCI 中自行启用。

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
