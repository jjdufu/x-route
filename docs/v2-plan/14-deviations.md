# 14 实施记录：与方案的偏差

按 01–11 实施时，以下各项与原方案不同。以代码为准；其余部分与方案一致。

## 发版与命名（01、11）

| 方案 | 实际 | 原因 |
|---|---|---|
| 新增 `ci.yml` / `release.yml`，打 `v*` 标签生成草稿 Release | 保留原有工作流文件名：`package-module.yml`（所有分支检查）与 `build-openwrt.yml`（push 到 `main` 时构建并发布） | 与历史发版方式保持一致；`run_number` 延续，标签为 `<version>-build.<N>` |
| 草稿 Release | 直接发布为 **prerelease**，正文第一条写明“尚未真机验证” | 历史 Release 由 CI 直接发布；prerelease 不会被当作正式版 |
| 版本 `v2.0.0-rc1` | `v2.0.0`（标签 `v2.0.0-build.<N>`） | 历史标签用 `-build.N` 后缀，不再叠加 `-rc1` |
| 只出一个 `x-route-<tag>.zip` | `x-route-sukisu-full-<ver>.zip` + `x-route-sukisu-lite-<ver>.zip`，另附 `Image`、`openwrt-rootfs.img.gz`、`openwrt.qcow2`、`sha256sums.txt` | 与历史 Release 资产一致 |
| 系统盘名 `openwrt-rootfs.raw(.gz)` | `openwrt-rootfs.img(.gz)` | 与历史资产名一致 |
| 模块名 | `x-route 随身软路由 (OnePlus 13)` | 去掉 v1 的“内核保活” |

## 内核（03）

- 配置基线改为 ImmortalWrt 25.12.2 SDK 里的官方最终 `.config`，再叠加 `openwrt/kernel-gunyah-pvm.config`。按 generic/armsr 片段拼接时缺少 IPv6 等依赖，`NFT_FIB_INET` 无法打开。
- 删除 `CONFIG_NFT_EXTHDR` / `CONFIG_NFT_RT`：6.12 中已并入 nf_tables 核心，不再是独立选项。
- 额外内建 `NF_CT_NETLINK`、`NFT_HASH`、`NFT_NUMGEN`、`NF_FLOW_TABLE(_INET)`、`NFT_FLOW_OFFLOAD`：避免 ImageBuilder 中对应 `.ko` 与内建符号冲突；同时关闭调试信息（`DEBUG_INFO_NONE`）。

## 固件（04）

- 使用 `dnsmasq-full` 代替 `dnsmasq`；额外排除 `automount`、`ppp`、`ppp-mod-pppoe`、`owut`、`luci-app-attendedsysupgrade`、`htop`。
- ImageBuilder 只开启 ext4 输出，取 `root.ext4`；排除 generic profile 自带的网卡 kmod。
- 生成后用 debugfs 后处理：写入本内核的 `modules.builtin(.modinfo)`，删除已内建的 `.ko` 以及依赖内建模块的 `.ko`（如 `nft_fullcone`），开机时 kmodloader 不再报错。
- `tune2fs -O ^resize_inode`：否则开机在线 `resize2fs` 报 `reserve_backup_gdb -22`。
- 防火墙关闭 `flow_offloading` 与 fullcone，lan 分区显式绑定 `device eth0`：flowtable 创建失败会导致 fw4 整体加载失败、input 链缺少 eth0。
- `imm_init.lang=1`：防止 ImmortalWrt 的 `99-default-settings` 把 LuCI 语言改回 auto。
- inittab 增加 `ttyAMA0::askfirst`（QEMU 调试控制台），`x-route-ready` 依次尝试 hvc0、ttyS0、ttyAMA0。

## 模块（05–10）

- `RW_MAP_OPT` 放在 `common.sh`（`valid_kv` 也要用），由 `share.sh` 读取。
- `net.sh` 中 `ip`/`ndc`/`iptables` 一律用 `/system/bin` 绝对路径：KernelSU 以 busybox ash 独立模式运行脚本，裸写 `ip` 会命中 busybox 的 ip 小程序（不支持 `tuntap`）。
- XR_FWD 的蜂窝 DROP 增加 `v4-ccmni+`。
- `valid_kv VPN_ROUTES` 除格式外也校验私网与主机位；`valid_kv VM_SHARE_PATH` 额外拒绝冒号、逗号、`$`、反引号和反斜杠。
- CIDR 工具函数只用 `_` 前缀变量（sh 无局部变量，原写法会覆盖调用方循环变量），且避免 mksh 32 位有符号移位比较。
- `xr set` 失败时返回 `{"ok":false,"error":"..."}`；`status-line` 在有未确认熔断时追加提示。

## 测试（新增）

- `tests/`：`test-common`、`test-install`、`test-vm`、`test-net`、`test-share`、`test-xr`、`test-webui`，在 mksh、busybox sh、dash 下运行；假 `crosvm` / `ip` / `ndc` / `iptables`，不触碰本机网络。
- `tests/e2e-qemu.sh`：把 crosvm 参数翻译成 QEMU virt/cortex-a72，用真实 `Image` 与系统盘做端到端冒烟（不进 CI）。
- `scripts/lint.sh`：shellcheck + `sh -n` + 全部单元测试 + 配置键检查；CI 安装 mksh、busybox。

## QEMU 中的实测结论（真机待验证）

- openvpn 2.7.8 显示 `[DCO]`，官方 `kmod-ovpn` 可加载；`apk add kmod-wireguard` 后 `modprobe` 成功（官方 kmod 与本内核兼容）。08、13 中“6.12 没有 DCO”的判断不成立，以真机日志为准。
- 首次开机 root 为空密码，LuCI（空密码登录 302）、ttyd、ssh 可用。
