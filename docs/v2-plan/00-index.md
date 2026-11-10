# x-route v2 重构实施文档（索引）

基线：`main` @ `f317884`（v1.0.1-build.5）。分支：`refactor/v2`。首个候选标签：`v2.0.0-rc1`（草稿 Release）。

规则：
- 每一步完成后先给用户确认，确认后才进入下一步；任何 push / tag / release 都需要用户确认。
- 只支持全新安装，不做 v1 升级、迁移、兼容和回滚逻辑。
- VM 侧只用 ImmortalWrt 25.12.2（apk、fw4/nftables）；手机侧 Android iptables 与此无关，保留。
- 每一步文档都有“验收”小节，全部通过才算完成。

| 步骤 | 文件 | 内容 |
|---|---|---|
| 01 | [01-ci-release.md](01-ci-release.md) | CI 与发布工作流：只认 `v*` 标签，只建草稿，删除虚假标签 |
| 02 | [02-repo-cleanup.md](02-repo-cleanup.md) | 删除旧文件，建立 v2 目录骨架 |
| 03 | [03-kernel.md](03-kernel.md) | 内核 6.12.103：25.12 armsr 配置 + Gunyah pVM 片段，构建与断言 |
| 04 | [04-firmware.md](04-firmware.md) | ImageBuilder 25.12.2：软件包清单、`files/`（uci-defaults、init.d、inittab） |
| 05 | [05-module-skeleton.md](05-module-skeleton.md) | 模块骨架：module.prop、customize.sh、service.sh、action.sh、uninstall.sh、common.sh、config.conf |
| 06 | [06-vm.md](06-vm.md) | vm.sh：crosvm 启停、熔断、磁盘扩容与重置 |
| 07 | [07-net.md](07-net.md) | net.sh：xr0、netd 本地网络、ipfwd、NAT、XR_FWD、XR_EXPOSE、对账循环 |
| 08 | [08-openvpn.md](08-openvpn.md) | OpenVPN：VM 侧 vpn 分区、VPN_MODE、手机侧手工网段路由 |
| 09 | [09-share.md](09-share.md) | 目录共享 ro/rw |
| 10 | [10-cli-webui.md](10-cli-webui.md) | `xr` 命令行与 WebUI |
| 11 | [11-packaging-readme.md](11-packaging-readme.md) | 打包、README、发布 rc1 |
| 12 | [12-device-tests.md](12-device-tests.md) | 真机测试清单 |
| 13 | [13-risks.md](13-risks.md) | 风险与待确认项 |

## 全局约定

手机侧路径：

| 名称 | 路径 |
|---|---|
| `MODDIR` | `/data/adb/modules/x_route` |
| `XR_DATA` | `/data/adb/x-route` |
| 配置 | `${XR_DATA}/config/config.conf` |
| 镜像 | `${XR_DATA}/images/Image`、`${XR_DATA}/images/openwrt-rootfs.raw` |
| 运行态 | `${XR_DATA}/run/`（pid、sock、console.log、vm-ctl.log、routes.active） |
| 日志 | `${XR_DATA}/log/xr.log`（超过 512KB 轮转为 `.1`） |
| 熔断 | `${XR_DATA}/config/vm_booting.fuse`、`tripped`、`last_good` |

网络：`172.29.233.0/24`，手机 `xr0` 用 `.1`，VM `eth0` 用 `.254`。

VM 默认登录：遵守上游，用户 `root`，空密码；不做任何修改。
