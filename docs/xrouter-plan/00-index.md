# XRouter 重构实践方案（索引）

基线：`main` @ `d0c8bfb`（已发布预发布 `v2.0.1-build.8`）。工作在本地 `main` 上进行，每完成一步提交一次详尽的中文 commit，全部测试通过后再快进推送到 `origin/main` 并由现有 CI 发预发布。

本目录是把已认同的完整方案，扩展为可按序落地的实践说明。`/workspace/xrouter-plan/` 与仓库 `docs/xrouter-plan/` 内容相同。

## 原则

- 破坏性更新，不做旧模块路径、旧配置、旧标识的兼容层或迁移。用户先卸干净再装。
- 对用户可见的文案用简洁优美的简体中文；不写解释性长文，不露出内部变量名，不带取值前缀（如 `hotspot：`、`terminal：`）。
- 提交说明用中文，写清改了什么、为什么。不写时间表或工作量。
- 不改 GitHub 仓库名（仍为 `jjdufu/x-route`，用户未定是否改名）。不 force-push `main`，不删除旧 Release / 标签。
- 不改用户手机，不动外部路由器。不在文件、提交说明、Release 或报告中泄露密钥、ovpn 私钥、VPN 服务器公网地址等秘密。
- root 保持上游默认空密码，不设成 `password`。
- 固件仍为 ImmortalWrt 25.12.2，apk + fw4/nftables；不预装 OpenList；OpenVPN 只预装 `openvpn-openssl`（不要 easy-rsa）。
- 卸载必须彻底：与模块有关的任何东西都不要留，包括日志、数据目录、规则、通知、服务、配置、镜像、运行时文件。
- 产品名改为 XRouter。路径与 id 用小写 `xrouter`，显示名与主机名用 `XRouter`。

## 命名对照

| 项目 | 旧 | 新 |
|---|---|---|
| 显示名 / 主机名 | x-route / x-route | XRouter |
| 模块 id / 模块目录 | `x_route` / `/data/adb/modules/x_route` | `xrouter` / `/data/adb/modules/xrouter` |
| 数据目录 | `/data/adb/x-route` | `/data/adb/xrouter` |
| 日志目录 | `/data/adb/x-route/log/` | `/data/adb/xrouter/log/` |
| 模块日志文件 | `xr.log` | `module.log` |
| 日志导出包 | `x-route-logs-…` | `xrouter-logs-YYYYMMDD-HHMMSS.zip` |
| 模块包名 | `x-route-sukisu-full/lite-<ver>.zip` | `xrouter-sukisu-full/lite-<ver>.zip` |
| 版本 | v2.0.1 / versionCode 201 | v3.0.0 / versionCode 300 |
| VM 内 init / uci-defaults | `x-route-net` 等、`10-x-route` | `xrouter-net` 等、`10-xrouter` |
| 唤醒锁 / ndc ipfwd 名 | `x-route` / `xroute` | `xrouter` / `xrouter` |

有意保留、不改的内部短名（不是对外标识）：

- CLI 仍叫 `xr`（短命令，避免无谓加长）。
- tap 网卡仍叫 `xr0`，iptables 链仍叫 `XR_FWD` / `XR_NAT`（仅运行时内部使用）。
- 环境变量前缀 `XR_`、`VM_` 仅用于配置键与脚本内部，不在界面上出现。

## 开发顺序与提交

每一步对应一次本地中文 commit。文档作为第 0 步单独提交。后一步依赖前一步的路径与行为，不得跳步。

| 步骤 | 文件 | 提交后的仓库状态 |
|---|---|---|
| 0 | 本目录全部文件 | 只有方案文档，代码未改 |
| 1 | [01-rename.md](01-rename.md) | 全部标识改为 XRouter，测试与打包路径同步更新 |
| 2 | [02-uninstall.md](02-uninstall.md) | 卸载脚本彻底清理 |
| 3 | [03-keepalive.md](03-keepalive.md) | 独立守护进程 + 常驻通知，VM 与 WebUI 生命周期解绑 |
| 4 | [04-gateway-mode.md](04-gateway-mode.md) | 运行模式「全局 / 旁路」，默认全局，防环路、防断网 |
| 5 | [05-openvpn.md](05-openvpn.md) | 模块可上传启用 ovpn，push 路由自动同步，固定网点方式 |
| 6 | [06-firmware.md](06-firmware.md) | 主机名、型号、包管理中文语言包；客侧重命名与网点防火墙与第 5 步对齐 |
| 7 | [07-webui.md](07-webui.md) | 四页 WebUI、自定义下拉与确认弹窗、文案精简 |
| 8 | [08-test.md](08-test.md) | 单元测试 / 能跑的 QEMU 冒烟，修问题并提交 |
| 9 | [09-release.md](09-release.md) | 全部通过后推 `main`、按现有 CI 发预发布 |

第 4、5 步会改手机侧网络与客侧防火墙；第 7 步只做模块 WebUI 与为界面服务的 CLI/导出，不改已冻结的网络语义。第 6 步的固件改动需要重新构建 rootfs 后，第 8 步的 QEMU 才能覆盖客侧部分。

## 配置键（重命名后的最终形态）

保留并出现在 `config.conf` 中的键：

`XR_NET_PREFIX` `XR_VM_MAC` `XR_DNS` `XR_EXPOSE` `XR_MODE` `VM_CPUS` `VM_MEM_MB` `VM_DISK_SIZE_MB` `VM_WAKELOCK` `VM_SHARE` `VM_SHARE_MODE` `VM_SHARE_PATH` `AUTOSTART`

从配置中删除、改由程序固定的项：

| 项 | 固定值 |
|---|---|
| 稳定期 | 180 秒 |
| swiotlb | 64 MB |
| 就绪超时 | 180 秒 |
| 系统盘可写 | 开 |
| 虚拟网卡 | 开 |
| 自启延迟 | 60 秒（不显示、不可配） |
| VPN 模式 / 手工网段 | 删除；按第 5 步自动处理 |

`XR_MODE`：`global`（默认）或 `bypass`。界面显示「全局接管」「旁路网关」。

`XR_EXPOSE` 取值为 `local` / `lan` / `all`，界面「管理访问」显示「本机」「内网」「所有」，默认内网；蜂窝入站一律拒绝。

## 不做的事项

- 不做检查更新。
- 不在模块或 OpenWrt 里做 Wi-Fi / 热点开关。
- 流量统计、包管理、改 root 密码、主题仍在 LuCI。
- 不预装 OpenList，模块不做 NAS 界面。
- 不显示 SSH，不做 SSH 端口映射。ttyd 仍可在固件中预装，但不进模块页。
- 不把仓库改名为 xrouter。

## 验收总则

- 每一步文件中的验收项全部通过，才允许提交并进入下一步。
- 第 8 步通过前，不得 push、不得打标签、不得发 Release。
- 不得编造测试结果。
