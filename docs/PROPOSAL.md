# x-route 项目立项书 (Project Charter & Proposal)

| 项目元数据 | 内容 |
| :--- | :--- |
| **项目名称** | **x-route**（OnePlus 13 虚拟化 OpenWrt 随身软路由与内核级保活系统） |
| **项目仓库** | [`https://github.com/jjdufu/x-route`](https://github.com/jjdufu/x-route) |
| **立项日期** | 2026-11-09 |
| **目标硬件平台** | OnePlus 13（高通骁龙 8 至尊版 SM8750，12GB/16GB/24GB RAM） |
| **目标系统环境** | ColorOS 15 / 16（基于 Android 15 / 16，GKI 2.0 Linux 6.6 内核）+ **SuKiSU** Root |
| **交付形态** | 1. 定制精简 ImmortalWrt ARM64 虚拟机镜像 (`Image` + `rootfs.img`)<br>2. 免 APK 的全功能 **SuKiSU 模块包 (`x-route-sukisu.zip`)**（含内置 WebUI）<br>3. 自动化编译流水线与详尽部署/排障手册 |
| **关联前置讨论** | 会话 `6fe161ac-9313-4e63-85c2-0d313b1e5033` |

---

## 一、 立项背景与核心痛点

在移动办公、差旅组网及随身私有云（NAS）场景中，用户希望直接使用已获取 SuKiSU 内核级 Root 权限的主力/备用旗舰手机（OnePlus 13），在不外挂任何硬件随身 Wi-Fi 设备的前提下，实现以下高阶网络诉求：

1. **全流量软路由化**：由一套拥有完整插件生态（LuCI、`opkg`、OpenVPN、分流插件、Alist 等）的 **OpenWrt 系统**统一接管手机网络；手机 5G/4G 蜂窝流量（`rmnet_dataX`）作为唯一 WAN 口，手机自身日常浏览及对外释放的 Wi-Fi 热点统一落在 OpenWrt 的 LAN 口。
2. **固定内网与热点拓扑**：彻底解决 Android 原生热点每次开启网段/IP 变动或无法自定义 DHCP/网关的问题，将 OpenWrt LAN 网关固定为 `192.168.100.1/24`，所有热点接入设备与手机本机均受 OpenWrt 统一管控。
3. **跨 CGNAT 异地组网互访**：在 5G 蜂窝无公网 IP 环境下，通过 OpenWrt 内置 OpenVPN 客户端主动连接家庭或机房服务端，实现“手机本机 + 热点下挂设备 ⇄ 家庭/机房内网”的双向透明互访。
4. **随身 NAS 存储共享与宿主强力保活**：手机本地存储中的共享文件夹（如 `com.leohao.android.alistlite` 托管的 NAS 目录）既可通过 SuKiSU 模块 WebUI 配置**四重内核级防杀/防冻结保活**，也可通过 `virtio-fs` / `virtio-9p` 直接挂载进 OpenWrt 内部运行原生 Alist 服务。

---

## 二、 项目核心目标 (Objectives)

### 2.1 功能目标
1. **定制极简 ImmortalWrt ARM64 固件**：
   - 适配高通 SM8750 **Gunyah Hypervisor (`/dev/gunyah`) Protected VM** 机制（开启 `CONFIG_DMA_RESTRICTED_POOL=y` 与 `CONFIG_SWIOTLB=y`），杜绝 900E 黑屏崩溃；
   - 剔除一切物理硬件冗余驱动与 PPPoE 组件，仅保留 VirtIO（`net`/`blk`/`pci`/`console`）、文件系统共享（`virtiofs`/`9p`）、防火墙（`firewall4`/`nftables` 及 `iptables-nft` 兼容层）、`luci`、`openvpn-openssl` + `luci-app-openvpn`、`alist` + `luci-app-alist`。
2. **免 APK 的纯 SuKiSU 模块化编排 (`x-route`)**：
   - 支持 **`crosvm`（Gunyah 硬件虚拟化，首选）** 与 **`qemu-system-aarch64`（MTTCG，备选）** 双引擎启动；
   - **WAN 动态守护**：实时监听 `rmnet_dataX` 蜂窝网卡切换与重连，自动维护 `tap-wan`（`172.16.1.0/24`）NAT 转发与 TTL=64 改写；
   - **LAN 热点无感接管**：基于 `ip monitor link` + `iw dev` 动态捕获热点接口（`ap0`/`wlan1`/`wlan2`），0.5 秒内完成 `tc` eBPF 卸载、宿主机 `NetworkStack` DHCP 屏蔽及 `br-lan` 二层桥接；
   - **本机流量安全回流**：通过防环路策略路由（`ip rule` + `suppress_prefixlength 0` + `fwmark 0x100/0x100` 豁免）与本机 DNS 53 端口重定向 + 强制关闭系统私人 DNS（DoT），让手机本机 App 成为 OpenWrt 的受管客户端。
3. **SuKiSU 原生 WebUI 与四重内核级保活引擎**：
   - 在 SuKiSU 管理器内提供可视化 WebUI：软路由状态监控、一键启停/切换本机回流开关、快捷跳转 LuCI（`http://192.168.100.1`）、NAS 目录挂载配置、以及**保活应用选择器**（默认预置 `com.leohao.android.alistlite`）；
   - 实现针对 Android 15/16 (ColorOS 15/16) 的四重保活：Doze/网络后台白名单、`oom_score_adj = -1000`、Cgroup v2 `cgroup.freeze=0` 解冻与根组迁移 + 禁用 Phantom Process Killer、看门狗静默唤醒。
4. **物理存储高速共享**：
   - 在 FBE 解密完成后，支持将 `/data/media/0/NAS` 通过 `virtio-fs` / `virtio-9p` 零拷贝挂载至 OpenWrt 内部 `/mnt/nas`。

### 2.2 非功能与性能指标 (SLAs)
- **固件体积**：内核 `Image` ≤ 12MB，压缩根文件系统 ≤ 35MB；
- **运行内存占用**：OpenWrt 虚拟机内存分配默认 `256MB`（可调至 `512MB`）；
- **热点接管时延**：手机设置开启热点后 **≤ 1.0 秒** 完成网桥接管，客户端 100% 获取 `192.168.100.X` 地址；
- **待机额外功耗**：空载待机 CPU 额外占用 **< 2%**，支持一加充电阈值控制（旁路/停充保护）。

---

## 三、 项目范围界定 (Scope)

| 属于本项目范围 (In-Scope) | 不属于本项目范围 (Out-of-Scope) |
| :--- | :--- |
| - OnePlus 13 (SM8750, Android 15/16, SuKiSU) 软路由模块开发与验证<br>- ImmortalWrt ARM64 极简固件配置与 GitHub Actions 自动编译<br>- SuKiSU 模块内置 WebUI 与四重内核级应用保活<br>- OpenVPN 客户端组网配置模板与 `/data/media/0/NAS` 目录直通<br>- 调试与部署操作手册（含 Termux 初期调试指南） | - 独立 Android Java/Kotlin APK 客户端开发（已由 SuKiSU WebUI + LuCI 替代）<br>- 应用商店上架或商业化分发<br>- 非 Root 设备或不支持虚拟化/命名空间的低端机型适配 |

---

## 四、 核心交付物清单 (Deliverables)

1. **D1：定制版 ImmortalWrt ARM64 编译工程 (`openwrt-config/` & `.github/workflows/`)**
   - `armsr-armv8.diffconfig`（精简软件包配置）
   - `kernel-gunyah-pvm.config`（开启 `CONFIG_DMA_RESTRICTED_POOL=y` 等虚拟化补丁）
   - `files/`（预置 `/etc/config/network` 固定 LAN `192.168.100.1`、WAN `172.16.1.2`、TTL 防检测规则及 `/mnt/nas` 自动挂载脚本）
   - GitHub Actions 云端一键编译工作流
2. **D2：SuKiSU 软路由与保活模块 (`module/`)**
   - `module.prop`、`customize.sh`、`service.sh`、`action.sh`、`uninstall.sh`
   - `scripts/vm_manager.sh`（`crosvm` Gunyah / `qemu` 双引擎生命周期管理）
   - `scripts/net_orchestrator.sh`（WAN 动态 NAT、热点 netlink 实时接管、本机策略路由回流与回滚）
   - `scripts/keepalive_daemon.sh`（四重内核级应用保活守护进程）
3. **D3：SuKiSU 模块内置 WebUI (`module/webroot/`)**
   - 响应式单页控制台（状态监控、模式切换、应用保活勾选、日志查看、旁路供电保护开关）
4. **D4：全套工程文档与操作手册 (`docs/`)**
   - 立项书、可行性与勘误报告、系统架构规范、真机刷入与 Termux 调试手册、OpenVPN 双向组网配置指南

---

## 五、 里程碑与阶段规划 (Milestones)

```mermaid
flowchart LR
    M0["M0: 立项与可行性勘误<br/>(已完成)"] --> M1["M1: ImmortalWrt 极简固件<br/>与 Gunyah 内核适配"]
    M1 --> M2["M2: SuKiSU 网络编排引擎<br/>(WAN/热点桥接/本机回流)"]
    M2 --> M3["M3: 模块 WebUI 面板<br/>与四重内核级保活 + NAS 挂载"]
    M3 --> M4["M4: OpenVPN 异地组网<br/>与真机长稳压测"]
    M4 --> M5["M5: CI 自动打包流水线<br/>与操作手册定稿"]
```

| 阶段 | 核心任务 | 关键产出与验收节点 |
| :--- | :--- | :--- |
| **M0：项目立项** | 评审会话 `6fe161ac` 方案、完成真机底层机制核查与五大勘误、建立仓库与立项文档 | `README.md`, `PROPOSAL.md`, `FEASIBILITY_ANALYSIS.md`, `ARCHITECTURE.md` |
| **M1：固件定制** | 编写 ImmortalWrt `armsr/armv8` 极简配置、注入 Gunyah pVM 受限 DMA 池内核选项、预置默认网络与挂载配置、配置 GitHub Actions 自动编译 | 产出可直接在 OnePlus 13 `crosvm`/`qemu` 启动且不触发 900E 的精简 `Image` 与 `rootfs.img` |
| **M2：网络引擎** | 实现 `tap-wan`/`tap-lan`/`br-lan` 编排、动态 `rmnet_dataX` 追踪、热点 `ap0/wlanX` 毫秒级接管、本机防环路策略路由回流与一键安全回滚 | 开启热点后外部设备自动获取 `192.168.100.X`；本机与热点流量均受控于 `192.168.100.1` |
| **M3：WebUI 与保活** | 开发 SuKiSU `webroot/` 界面、实现四重内核级保活脚本（含 Cgroup v2 解冻与幽灵进程豁免）、完成 `/data/media/0/NAS` 的 `virtio-fs`/`9p` 挂载 | `com.leohao.android.alistlite` 锁屏息屏 24 小时零被杀、零断网；OpenWrt `/mnt/nas` 正常读写 |
| **M4：组网与压测** | 联调 OpenVPN 客户端与服务端 `ccd` (`iroute`) 双向互访、配置 Wakelock 与充电保护、进行基带切换与热点反复开关压测 | 异地内网与热点下挂设备双向 ping/HTTP 互通；连续运行 72 小时无内存泄漏或断流 |
| **M5：交付归档** | 完善一键打包 `.zip` 脚本与 Termux 调试/日常维护全流程手册 | 发布 Release v1.0.0 可刷入 SuKiSU 模块包与完整文档 |

---

## 六、 风险评估与应对策略 (Risk Matrix)

| 风险项 | 发生概率 | 影响程度 | 缓解与兜底预案 |
| :--- | :---: | :---: | :--- |
| **Gunyah pVM 下 VirtIO DMA 访问异常导致 900E 黑屏** | 高（若用默认内核） | 致命 | 在 M1 阶段强制启用 Guest 内核 `CONFIG_DMA_RESTRICTED_POOL=y` 与 `CONFIG_SWIOTLB=y`，并提供 `Droid-VM/crosvm` 与 `QEMU MTTCG` 双后端兜底 |
| **开机运行一段时间后物理大页碎片化导致 Gunyah 启动 OOM** | 中 | 高 | 1. 精简 OpenWrt 内存至 `256MB`；2. 在 `service.sh` 开机早期启动；3. 可选集成 `gh-hugepage-reserve` 预留池 |
| **本机策略路由回流错误导致手机断网或死循环** | 中 | 高 | 1. 默认提供“仅接管热点”与“接管热点+本机回流”双模式切换；2. `action.sh` 提供一键紧急清理路由表并恢复原生网络；3. 严格通过 `suppress_prefixlength 0` 保护直连网段 |
| **ColorOS 系统升级改变热点网卡名或 Cgroup 路径** | 中 | 中 | 采用 `iw dev` 动态匹配 `type AP` 而非硬编码 `ap0`；通过 `/proc/$PID/cgroup` 动态反查进程所属 Cgroup 路径进行解冻 |
| **长时间运行发热与电池损耗** | 中 | 中 | 默认仅分配 2 个能效核/虚拟核，开启 Wakelock 同时保留 CPU idle 浅休眠，并在 WebUI 集成充电上限控制（60%~70% 停充旁路） |
