# x-route

> **OnePlus 13 (Snapdragon 8 Elite / Android 15·16 / SuKiSU) 虚拟化 OpenWrt 随身软路由与内核级保活系统**

[![Platform](https://img.shields.io/badge/Platform-OnePlus%2013%20(SM8750)-red)](https://github.com/jjdufu/x-route)
[![Android](https://img.shields.io/badge/Android-15%20%2F%2016%20(GKI%206.6)-green)](https://github.com/jjdufu/x-route)
[![Root](https://img.shields.io/badge/Root-SuKiSU%20%2F%20KernelSU-blue)](https://github.com/jjdufu/x-route)
[![Status](https://img.shields.io/badge/Status-Project%20Initiated%20(立项阶段)-orange)](./docs/PROPOSAL.md)

---

## 📖 项目简介

**x-route** 旨在无需任何外置硬件的前提下，将已通过 **SuKiSU** 获取内核级 Root 权限的 **OnePlus 13（骁龙 8 至尊版 SM8750，Android 15/16）** 改造为一台**全功能、固定网关、低功耗的随身 OpenWrt 软路由与便携式私有云（NAS）节点**。

通过高通底层硬件虚拟化（**Gunyah Hypervisor `/dev/gunyah` + `crosvm`**，兼容 QEMU 备选后端）运行极致精简的独立 **ImmortalWrt (ARM64)** 系统：
- **WAN 口**：动态绑定手机 5G/4G 蜂窝数据网络（`rmnet_dataX`），支持基带重连漂移自愈与 TTL 防检测；
- **LAN 口**：无感接管系统 Wi-Fi 热点（固定 `192.168.100.0/24` 网段与 `192.168.100.1` 网关），并支持手机本机应用流量回流至 OpenWrt 统一分流/去广告/策略路由；
- **异地组网**：内置 OpenVPN 客户端支持，轻松穿透运营商 CGNAT 与家庭/机房内网实现双向互访（`iroute`）；
- **存储互通与后台保活**：支持通过 `virtio-fs` / `virtio-9p` 将手机物理存储（`/data/media/0/NAS`）直通挂载至 OpenWrt，同时提供 **SuKiSU 模块 WebUI** 与 **四重内核级应用保活引擎**（针对 `com.leohao.android.alistlite` 等服务防杀、防冻结、防 Doze 断网）。

---

## 📂 文档导航（立项与架构文档）

| 文档 | 说明 |
| :--- | :--- |
| 📋 **[项目立项书 (PROPOSAL.md)](./docs/PROPOSAL.md)** | 项目背景、核心目标、交付物清单、里程碑规划（M1~M5）、验收标准与风险矩阵 |
| 🔬 **[可行性分析与技术勘误报告 (FEASIBILITY_ANALYSIS.md)](./docs/FEASIBILITY_ANALYSIS.md)** | 针对前期方案讨论（会话 `6fe161ac`）的深度工程可行性评估与 **5 大致命技术陷阱勘误** |
| 🏗️ **[系统架构与技术规范 (ARCHITECTURE.md)](./docs/ARCHITECTURE.md)** | 网络拓扑、策略路由表设计、热点动态接管机制、OpenWrt 精简编译配置与 SuKiSU 模块规范 |
| 💬 **[前期讨论原始记录](./docs/references/OnePlus13_OpenWrt_Discussion_6fe161ac.md)** | 会话 `6fe161ac-9313-4e63-85c2-0d313b1e5033` 原始技术讨论存档 |

---

## ⚡ 核心可行性结论与关键技术修正摘要

在立项调研中，我们对前期讨论方案进行了严格的真机环境与内核级交叉验证，结论为：**整体需求 100% 可实现，但原讨论方案存在 5 处关键技术事实偏差，已在立项架构中完成修正**：

1. **虚拟化底座修正（`/dev/gunyah` vs `/dev/kvm`）**：
   - **问题**：高通骁龙 8 至尊版（SM8750）量产机由高通自研 **Gunyah Hypervisor** 独占 EL2，宿主机内核运行于 EL1，系统仅暴露 `/dev/gunyah` 而**不存在 `/dev/kvm`**；且 Gunyah 强制启用 Protected VM (pVM) 内存借出机制，普通 VirtIO 驱动未开启受限 DMA 池会直接触发手机 **900E 黑屏死机**。
   - **修正**：主引擎采用 **`crosvm` (`--hypervisor gunyah --protected-vm-without-firmware`)**，并在自编译 OpenWrt 内核中强制开启 `CONFIG_DMA_RESTRICTED_POOL=y` 与 `CONFIG_SWIOTLB=y`；同时保留 QEMU MTTCG 作为跨机型兼容后备。
2. **热点 DHCP 接管修正（`NetworkStack` vs `dnsmasq`）**：
   - **问题**：Android 10+ 已彻底移除 `dnsmasq`，Android 15/16 的热点 DHCP 由 `com.google.android.tethering` 进程内的 Java `DhcpServer` 托管，并挂载了 `tc` eBPF 卸载程序。原方案 `killall -9 dnsmasq` 无效。
   - **修正**：通过 `iw dev` 动态识别 AP 接口（`ap0`/`wlan1`/`wlan2`），卸载接口 `tc clsact` eBPF 规则，利用 `ebtables`/`iptables raw` 阻断宿主本地 UDP 67/68 响应，并将 AP 接口二层桥接至 `br-lan`，由 OpenWrt 全权接管 DHCP/DNS。
3. **本机流量回流防环路修正（Android `netd` `fwmark` 兼容）**：
   - **问题**：Android `netd` 对所有 Socket 均打有复杂 `fwmark`，原方案 `ip rule add not fwmark 0x100` 会劫持系统连通性探测与局域网直连路由，引发路由死循环。
   - **修正**：采用高优先级 `suppress_prefixlength 0` 保护直连网段，使用 `fwmark 0x100/0x100` 掩码 + GID/UID 豁免虚拟机自身与基带控制面流量，并通过 `OUTPUT` 链重定向本机 DNS（53 端口）并关闭系统 Private DNS（DoT）。
4. **物理存储直通修正（`/data/media/0` vs `/sdcard` FUSE）**：
   - **问题**：`/sdcard` 位于 Android FUSE 层，存在挂载命名空间隔离与高 I/O 开销。
   - **修正**：在 FBE 解密完成后直通底层物理路径 `/data/media/0/NAS`，优先采用 `virtio-fs`（兼容 `virtio-9p`）挂载至 OpenWrt `/mnt/nas`。
5. **ColorOS 15/16 四重保活强化**：
   - **问题**：ColorOS 的 Hans/OPlus Freezer 与 Cgroup v2 `cgroup.freeze=1` 无法仅靠 `SIGCONT` 解冻。
   - **修正**：结合 Doze/网络白名单、`oom_score_adj = -1000`、Cgroup v2 `cgroup.freeze = 0` + 根组迁移、禁用 Phantom Process Killer 及 `am start-foreground-service` 静默守护。

---

## 🗂️ 仓库目录规划

```text
x-route/
├── README.md                          # 项目主页与概览
├── docs/
│   ├── PROPOSAL.md                    # 项目立项书（背景、目标、里程碑、风险控制）
│   ├── FEASIBILITY_ANALYSIS.md        # 深度可行性分析与原方案勘误报告
│   ├── ARCHITECTURE.md                # 系统架构、网络拓扑与模块设计规范
│   └── references/
│       └── OnePlus13_OpenWrt_Discussion_6fe161ac.md
├── openwrt-config/                    # ImmortalWrt ARM64 极简定制配置与补丁（M1 阶段交付）
├── module/                            # SuKiSU 模块源码（service.sh, action.sh, 网络与保活引擎，M2/M3 交付）
│   └── webroot/                       # SuKiSU 模块内置 WebUI 管理前端（M3 阶段交付）
└── scripts/                           # 固件自动构建与模块打包脚本
```

---

## 🗺️ 研发路线图 (Roadmap)

- [x] **M0：技术调研、可行性论证与项目立项**（已完成）
- [x] **M1：ImmortalWrt ARM64 极简固件定制与 Gunyah pVM 内核适配**（已完成）
- [ ] **M2：SuKiSU 核心网络编排引擎（5G WAN 动态 NAT + 热点无感桥接 + 本机回流）**
- [ ] **M3：SuKiSU WebUI 控制面板 + 四重内核级保活引擎 + NAS 目录直通**
- [ ] **M4：OpenVPN 异地组网联调 + 旁路供电/温控/唤醒锁稳定性压测**
- [ ] **M5：一键构建流水线（GitHub Actions）与完整实操手册发布**
