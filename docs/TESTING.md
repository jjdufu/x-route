# XRouter 真机测试清单

只在用户的测试手机上，并且得到用户同意后执行。每一项都记录：结果（通过 / 失败）、相关日志片段（设置 → 日志 → 日志导出 / 诊断导出，或 `xr log-export` / `xr diag-export`，导出到 Download 的 .zip）、发现的问题。不记录 IMEI、序列号、证书和密码。

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
4. 在 WebUI 点主页的电源按钮：
   - 顶部标题栏固定，底栏导航为：状态 · 网络 ·［分流］· 系统（「分流」仅在 OpenClash 运行且启用「分流管理」时出现；日志在系统页的「日志」分组里导出）；点击或左右滑动切换时标题不动；
   - 启动中电源按钮外圈转动，依次显示「启动虚拟机 → 网络就绪 → 运行中」；`READY_TIMEOUT_SEC` 内出现 `XR_READY`，记录走的是 hvc 还是 ttyS0；
   - 启动完成后按钮呈绿色（无呼吸光），标题栏状态为「运行中」（不要出现「已稳定」）；后台在 `STABLE_SEC` 后写入 `last_good`；
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

## 8 状态页与实时数据

1. 412×915 与 360×640 下状态页一屏放下、无滚动；不出现 OpenClash、分流、IPv6、组网、热点卡片。
2. 实时上传 / 下载与手机下载速度一致（`xr status-json` 的 `traffic.rate`）；`kill` 掉 daemon 后磁贴显示「—」，`status-json` 不卡顿。
3. OpenWrt 负载显示 CPU%，副标题为内存与 load；在线设备为「本机 1 · 热点 N」：开热点并接入一台设备后为 2。设备离开后要过几分钟（邻居表老化）才减少。
4. 累计流量随状态轮询刷新；关闭 WebUI 后 `guest/stats` 不再更新（只按需上报）。

## 9 配置备份

1. 服务未运行：系统页「配置备份」两个按钮禁用，说明行为「服务未运行，需先启动」。
2. 运行中点「导出配置」：10 秒内提示 `Download/xrouter-config-<本地时间>.zip`；解压可见 manifest.json、phone/config.conf、phone/vpn/*、openwrt/backup.tar.gz。
3. 修改设置、删一个 VPN 配置、LuCI 改主机名后点「导入配置」：文件浏览器只列目录和 .zip，从 Download 开始；确认框提示明文、含密码与密钥；确认后服务自动重启，全部恢复，VPN 自动连上。
4. 坏包：改坏 zip 的一个字节后导入，提示「校验失败」，配置不变、服务不重启。
5. 旧系统盘：两个按钮禁用，说明行为「当前系统盘不支持」；命令行导出提示「当前系统盘不支持配置备份」。

## 10 日志与诊断

1. 时区：导出文件名、`module.log`、诊断包 `summary.txt` 都是手机本地时间（+0800）。
2. 看门狗：在 OpenWrt 内执行 `reboot` 3 次，都没有 ERROR；`kill -9 crosvm` 时仍立即撤路由。
3. 诊断降噪：诊断包的 nft 段是 terse 输出并附元素计数，有「监听端口」段；打开调试模式后 `debug.log` 中没有幂等删除（如 `ip rule del` 不存在）的噪声。
4. dropbear：记录「监听端口」段里 dropbear :22 的监听地址，作为观察项留档。

## 11 写盘与长稳

1. 写盘：熄屏 5 分钟，`run/` 和 `config/` 下没有周期性写入。测量方法：熄屏前后各执行一次 `cat /proc/$(pidof xr | cut -d' ' -f1)/io`（对 daemon 进程），记录两次 `write_bytes` 与时间差；配合 `ls -l --time-style=full-iso /data/adb/xrouter/run /data/adb/xrouter/config` 核对修改时间。
2. 长稳：运行 2 小时，期间熄屏、亮屏、切换网络、开关热点各若干次；没有整机复位，没有新增 ERROR（只允许真实故障）。结束后 `xr diag-export` 留档，并保存状态页两种尺寸的截图。

## 建议的首测顺序

1. 第 0 节前置检查（只读）。
2. 第 1 节安装与第一次手动启动：重点看是否整机复位、`XR_READY` 走的通道。
3. 第 4 节手机访问 LuCI、VM 上网、停止后规则清空。
4. 第 2 节 1 小时稳定性，然后再开自启。
5. 其余各节。
6. 之后依次执行 8 → 9 → 10 → 11。

## 附：OpenVPN / Clash 能力核对（VM 内，可离线）

把仓库里的 `tests-sh/vm-caps.sh` 拷进 VM 执行（`ssh root@172.29.233.254 sh -s < tests-sh/vm-caps.sh`），最后一行应为 `CAPS_OK`。
它核对：预装官方 kmod 可 modprobe（tun、nft_tproxy、nft_socket、nft_fullcone、inet_diag、ovpn 等）；
socket / tproxy / redirect / masquerade / fullcone 等 nft 规则能加载；fwmark 策略路由、TUN、IPv6 转发可用；
fw4 的 fullcone 与 flow offloading 可以开启；内核日志无模块兼容错误。

插 5G 卡、关闭 Wi-Fi 时另外确认：VM 内 `ping 223.5.5.5`、`curl -I https://downloads.immortalwrt.org` 成功；
手机 `iptables -S XR_FWD` 中有 `-i xr0 -o rmnet_dataX -j ACCEPT` 与 `-i rmnet_dataX -o xr0 -j DROP`，
纯 IPv6 运营商（464xlat）时还应出现 `v4-rmnet_dataX` 的同类规则和 MASQUERADE。


## 附：构建与 Rust 门禁

- 工具链由根目录 `rust-toolchain.toml` 锁定为 1.100.0（rustup 自动选择，不使用 `cargo +<版本>`），edition 2024。
- Rust 门禁入口：`bash scripts/gate.sh`（工具链核对、`lint-guard.py`、fmt、clippy、文档、测试），另加交叉目标
  `cargo clippy --workspace --all-targets --all-features --locked --target aarch64-unknown-linux-musl -- -D warnings`。
  标准见 [`docs/门禁-Rust最严Lint.md`](门禁-Rust最严Lint.md)。本项目额外开启 `clippy::cognitive_complexity = "deny"`（阈值 15，D3-B），这一项不写进门禁文档。
- 不得用任何方式压制或降级告警（`allow`/`expect`/`cfg_attr`、降级 lint 级别的编译器参数、`RUSTFLAGS` 等）。
- Rust 测试：`bash scripts/gate.sh` 已包含 `cargo test --workspace --all-targets --all-features --locked`。测试目录如下：
  - `rust/xr/src/**/tests.rs`：各模块单元测试，含属性测试。
  - `rust/xr/tests/<名称>/`：集成测试，包括 `diag_export`、`diag_format`、`diag_hosts`、`human_bytes`、`net_traffic`、`prop_core`、`status_json`、`uninstall`；`fixtures/` 里是金样。
  - `rust/test-support/`：测试辅助 crate `xr_test_support`（`Failure`、`ensure*`）。
  - 金样用 `XR_GOLDEN_RECORD=1` 重录。
- 计数门禁：`python3 scripts/gate-count.py --check`，基线是 `gate-baseline.json`，目前 TOTAL 0，一次性跳变已用掉。
- 产物路径不变：`rust/xr/target/aarch64-unknown-linux-musl/release/xr`（根目录 `.cargo/config.toml` 的 `target-dir` 指回 `rust/xr/target`）。

## 附：本机已跑过的检查（发版前）

环境：Grok Bot box，Asia/Shanghai。下列结果不得用来代替真机测试。

| 检查 | 结果 |
|---|---|
| `sh scripts/lint.sh`（shellcheck、`sh -n`、全部 `tests-sh/test-*.sh` 在 dash / mksh / busybox sh 下） | 通过 |
| `sh scripts/check-config-keys.sh` | 通过（19 个配置键） |
| `bash scripts/package-module.sh --lite-only` | 生成 `xrouter-sukisu-lite-v3.0.0.zip`：`id=xrouter`，无旧路径，无 META-INF |
| `tests-sh/test-webui.sh`（node） | 通过 |
| `tests-sh/e2e-qemu.sh` | 通过（先用当前源码重新构建 rootfs，再启动；已核对主机名 XRouter、型号、zh_cn、fw4、OpenVPN、透明代理能力） |

本次（v4.x，2026-11-15，Rust 1.100.0 工具链，edition 2024）：

| 检查 | 结果 |
|---|---|
| `bash scripts/gate.sh`（由 `sh scripts/lint.sh` 调用） | 「gate: 全部通过」 |
| `cargo clippy … --target aarch64-unknown-linux-musl -- -D warnings`、`cargo build --release --locked --target aarch64-unknown-linux-musl` | 通过 |
| `python3 scripts/gate-count.py --check` / `python3 scripts/lint-guard.py` | TOTAL 0 / 无输出 |
| `sh scripts/lint.sh`（全部 `tests-sh/test-*.sh` 在 dash / mksh / busybox sh 下，含金样比对） | 通过（25 个配置键） |
| `npm ci` + `npm run build`；`tests-sh/test-webui.sh` | 无警告；通过 |
| `bash scripts/package-module.sh --lite-only` | 生成 `xrouter-sukisu-lite-v4.1.2.zip`：`id=xrouter`，无 META-INF |

