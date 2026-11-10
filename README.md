# x-route

> 在骁龙手机的 Gunyah 受保护虚拟机（AVF pVM）里运行 **ImmortalWrt 25.12**，给手机本机和热点设备提供 OpenWrt 网关与 OpenVPN 客户端。SukiSU / KernelSU 模块，自带 WebUI。

[![Platform](https://img.shields.io/badge/Platform-OnePlus%2013%20(SM8750)-red)](https://github.com/jjdufu/x-route)
[![Root](https://img.shields.io/badge/Root-SukiSU%20%2F%20KernelSU-blue)](https://github.com/jjdufu/x-route)
[![Firmware](https://img.shields.io/badge/ImmortalWrt-25.12.2-green)](https://github.com/jjdufu/x-route)
[![Status](https://img.shields.io/badge/Status-v2%20Pre--release%20(未经真机验证)-orange)](./docs/TESTING.md)

> **v2 是一次完整重写，目前只通过了单元测试与 QEMU 启动冒烟测试，尚未在真机上验证。** 请先按 [docs/TESTING.md](./docs/TESTING.md) 测试，再日常使用。

## 1. 是什么

- VM：ImmortalWrt 25.12.2（armsr/armv8 ImageBuilder，包管理器 `apk`），内核 Linux 6.12.103（打上 ImmortalWrt generic/armsr 补丁，使用官方内核配置 + Gunyah pVM 片段），可直接 `apk add` 官方 25.12 的 `kmod-*`。
- 预装：LuCI（简体中文）、ttyd、OpenVPN（`openvpn-openssl` + `luci-app-openvpn`）、tcpdump、dnsmasq-full。
- 手机侧：tap 网卡 `xr0` 接入 VM，登记为 netd 本地网络；NAT 与转发规则只针对 VM 网段，蜂窝网络永不对外放行。

## 2. 要求

- 支持 AVF pVM 的骁龙机型，存在 `/apex/com.android.virt/bin/crosvm`（开发机型：OnePlus 13）。
- SukiSU / KernelSU。
- `/data` 剩余空间至少 1.5GB。

## 3. 安装

- **只支持全新安装**：装过 v1 的，先在管理器里卸载旧版并重启，再刷入 v2。v1 的配置不会迁移。
- 发布文件：
  - `x-route-sukisu-full-<版本>.zip`：模块 + 内核 `Image` + 系统盘 `openwrt-rootfs.img.gz`，首次安装用它；
  - `x-route-sukisu-lite-<版本>.zip`：只含脚本和 WebUI，要求 `/data/adb/x-route/images/` 下已有镜像；
  - `Image`、`openwrt-rootfs.img.gz`、`openwrt.qcow2`、`sha256sums.txt`：单独的固件文件与校验和。
- 安装后**默认不自启**，在 WebUI 点“启动”。

## 4. 首次使用

- 浏览器打开 `http://172.29.233.254`（LuCI），终端 `http://172.29.233.254:7681`（ttyd），或 `ssh root@172.29.233.254`。
- 用户 `root`，**空密码（上游默认）**。热点设备默认也能访问 VM，请尽快在 LuCI → 系统 → 管理权 中设置密码。

## 5. 网络

- 地址规划：`172.29.233.0/24`，手机 `xr0` 为 `.1`，VM `eth0` 为 `.254`（网关 `.1`）。前缀可通过 `XR_NET_PREFIX` 修改。
- `XR_EXPOSE`（谁能访问 VM 网段）：
  - `off`：只有手机本机；
  - `hotspot`（默认）：再加热点 / USB 共享的设备；
  - `all`：再加 Wi-Fi 上游所在局域网，需要在上级路由器添加静态路由 `172.29.233.0/24 via <手机 IP>`。
- VM 经手机当前默认上游（netd 选择的网络）上网；蜂窝接口进入 `xr0` 的流量一律丢弃。
- netd 的 `ndc` 不可用时自动退回 table 97 + 策略规则，WebUI 状态里显示“退回模式”。

## 6. OpenVPN

- 在 rtvpn 里为本设备**单独签发**客户端证书，在 LuCI → VPN → OpenVPN 上传 `.ovpn`（p12 版本另传 `.p12` 到 `/etc/openvpn/`）并启用。
- `VPN_MODE`（改后需重启 VM）：
  - `terminal`（默认）：普通客户端，所有来源伪装成 VM 的隧道地址；
  - `site`：网点网关，对端宣告 `172.29.233.0/24` 后可以反向访问手机侧。
- `VPN_ROUTES`：在 WebUI 手工填写对端内网网段（每行一个 CIDR），手机才会把这些网段路由进 VM。VM 不向手机上报任何路由。
  - 规则：必须是私网地址（10/8、172.16/12、192.168/16、100.64/10），主机位为 0，不能与 VM 网段、当前上游或热点网段重叠，最多 64 条；
  - 不合法的网段会显示在“被拒绝”里，不会下发。

## 7. 目录共享

- 默认关闭。开启后手机目录经 virtiofs 挂载到 VM 的 `/mnt/sdcard`。
- 路径只能是 `/data/media/0` 或其子目录（底层路径，不走 FUSE）；手机未解锁时本次启动不共享。
- `ro`（默认）：在 crosvm 自己的挂载命名空间里做只读绑定，手机侧内核强制只读，Android 侧看不到这次挂载。
- `rw`：只有当前 crosvm 支持属主映射时才可用（否则 WebUI 置灰）。VM 写入的文件不经过 MediaStore，相册、文件管理器需要手动扫描或重启后才能看到；不要写入 `Android/data`、`Android/obb`。

## 8. 安全措施

- 启动熔断：启动 VM 时写入 `vm_booting.fuse`，若在就绪/稳定前整机复位，开机后自动关闭自启并在 WebUI 显示熔断条，确认后才能再启动。
- 自启门槛：必须先用当前配置手动启动并运行到“已稳定”，才能打开开机自启；影响稳定性的配置一改，门槛重新生效。
- 保守的 VM 参数：`--no-balloon`、swiotlb 64MB、内存 512MB、`ulimit -l unlimited`。
- 就绪超时（默认 180 秒）内没有收到 VM 的 `XR_READY`，自动停止 VM。

## 9. 命令行

```sh
XR="sh /data/adb/modules/x_route/scripts/xr"
$XR start | stop | running          # 启动 / 停止 / 是否运行（返回码）
$XR status-json | status-line       # 状态
$XR get                             # 列出全部配置
$XR set KEY VALUE                   # 修改配置（会校验，返回 {"ok":...,"restart":...}）
$XR ack-trip                        # 确认熔断
$XR expand MB | reset-disk          # 扩容 / 重置系统盘（需先停止 VM）
$XR sync                            # 立即重建手机侧网络规则
$XR log [xr|console|ctl|vmm] [N]    # 查看日志
```

## 10. 构建

依赖与 CI（`.github/workflows/build-openwrt.yml`）相同：Debian/Ubuntu 上的 `build-essential bc bison flex libssl-dev libelf-dev gcc-aarch64-linux-gnu zstd e2fsprogs qemu-utils zip` 等。

```sh
bash scripts/build-kernel.sh      # 内核 Image -> dist/openwrt/
bash scripts/build-rootfs.sh      # ImageBuilder 系统盘 -> dist/openwrt/openwrt-rootfs.img.gz、openwrt.qcow2
bash scripts/package-module.sh    # 模块 zip、sha256sums.txt、RELEASE_NOTES.md
sh scripts/lint.sh                # shellcheck + 单元测试（mksh / busybox sh / dash）
sh tests/e2e-qemu.sh              # 可选：用 QEMU 启动真实镜像做端到端冒烟
```

推送到 `main` 后由 CI 构建并发布 Release（标签 `<版本>-build.<序号>`）。

## 11. 日志与卸载

- 日志：`/data/adb/x-route/log/xr.log`、`vmm.log`；VM 串口：`/data/adb/x-route/run/console.log`、`vm-ctl.log`。
- 卸载：在管理器里卸载模块并重启，会删除 `/data/adb/x-route`（**包括 VM 系统盘**），并清除手机侧的网络规则。

## 12. 已知限制

- v2 尚未在真机上验证，见 [docs/TESTING.md](./docs/TESTING.md)。
- v1 出现过的整机复位根因仍未确认；保留了上面全部安全措施，复位时按测试清单逐项二分。
- 依赖 netd 的默认网络规则格式（`fwmark 0x0/0xffff iif lo lookup <iface>`），Android 大版本升级后需要重新检查。
- virtiofs 共享在 pVM 上未经测试，可能较慢或不可用；rw 依赖 crosvm 的属主映射支持。
- `VPN_ROUTES` 只能手工维护。
- 只支持官方源中的 apk 包，不支持只有 ipk 的第三方包。
