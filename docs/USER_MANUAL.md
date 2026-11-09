# x-route 部署、调试与日常使用全流程操作手册

> **适用机型**：OnePlus 13（骁龙 8 至尊版 SM8750，Android 15/16 · ColorOS 15/16，已刷入 **SuKiSU** 获取 Root 权限）

---

## 一、 快速上手（刷入即用流程）

### 1. 获取安装包与固件
在 GitHub Releases 或 Actions 构建产物中获取：
- **方式 A（全量一体包，推荐）**：下载 `x-route-sukisu-full-v1.0.0.zip`（内置编译好的 Gunyah pVM 适配版 OpenWrt 内核 `Image` 与精简根文件系统 `openwrt-rootfs.img.gz`）。
- **方式 B（轻量模块包 + 独立镜像）**：下载 `x-route-sukisu-lite-v1.0.0.zip`，并将 `Image` 与 `openwrt-rootfs.img.gz` 手动放入手机 `/data/adb/x-route/images/` 目录。

### 2. 在 SuKiSU 管理器中刷入模块
1. 打开手机上的 **SuKiSU 管理器 App** -> 进入 **“模块 (Modules)”** 页面；
2. 点击 **“从本地安装”**，选择 `x-route-sukisu-full-v1.0.0.zip`；
3. 安装脚本会自动检测一加 13 的 `/dev/gunyah` 硬件虚拟化节点与 `/apex/com.android.virt/bin/crosvm`，并将镜像解压至 `/data/adb/x-route/images/`；
4. 重启手机（或无需重启，直接在模块卡片上点击 **“操作 (Action)”** 或 **“WebUI”** 按钮启动）。

---

## 二、 日常使用与管理方式

### 1. 使用 SuKiSU 模块内置 WebUI 面板
在 SuKiSU 管理器模块列表中，点击 **x-route** 卡片上的 **“WebUI”** 按钮，即可打开原生控制台：
- **仪表盘**：实时查看 OpenWrt 虚拟机状态（Gunyah `crosvm` / `QEMU`）、5G 出口网卡（`rmnet_dataX`）、Wi-Fi 热点网桥状态、LAN 下挂设备数；支持一键启停软路由、一键切换**“手机本机流量回流”**。
- **应用保活**：默认已内置保护 `com.leohao.android.alistlite`；可点击 **“扫描手机已安装第三方应用”** 勾选任意需要四重内核保活的 App，并实时查看其 `PID` 与 `oom_score_adj (-1000)` 状态；支持开启**一加 13 旁路/停充保护（电量 ≥70% 停充，≤60% 恢复）**。
- **路由与 NAS 配置**：自由切换虚拟化后端（`auto` / `gunyah` / `qemu`）、vCPU 核数（`2`/`4`）、内存大小（`256MB`~`512MB`）以及本机共享目录路径（默认 `/data/media/0/NAS`）。
- **运行日志**：在线查看 `x-route` 编排日志、OpenWrt 串口控制台启动日志与宿主策略路由诊断信息。

### 2. 进入 OpenWrt LuCI 网页管理后台
软路由启动后，无论是**在手机本机自带浏览器**，还是**在连接了手机 Wi-Fi 热点的电脑/平板浏览器**中，直接访问：
- **OpenWrt LuCI 后台**：`http://192.168.100.1`（默认账号 `root`，默认无密码）
- **OpenList (Alist) 私有云**：`http://192.168.100.1:5244`

### 3. 开启随身 Wi-Fi 热点
1. 直接在手机系统设置（或下拉控制中心）打开 **“个人热点”**；
2. `x-route` 的后台 Netlink 监听进程会在 **0.5 秒内** 自动检测到热点接口（`ap0` / `wlan1`）拉起，自动剥离系统自带 DHCP 与 eBPF 规则，并将其桥接至 OpenWrt 的 `br-lan`；
3. 任何连接手机热点的外部设备都会自动获取 **`192.168.100.100 ~ 192.168.100.250`** 网段的固定内网 IP，网关与 DNS 恒定为 **`192.168.100.1`**。

---

## 三、 本机 NAS 目录共享与挂载指南

`x-route` 同时支持两种方式共享手机存储中的 NAS 文件夹：

### 方案 A：继续使用手机端 `com.leohao.android.alistlite`（配合四重内核保活）
1. 在 SuKiSU WebUI 的 **“应用保活”** 页面确认 `com.leohao.android.alistlite` 处于受保护列表中；
2. 守护进程会每 25 秒执行一次：
   - 豁免 Doze 休眠与后台断网限制（`deviceidle whitelist` + `netpolicy`）；
   - 关闭 Android 15/16 幽灵进程杀手（`max_phantom_processes`）；
   - 将 Alist Lite 主进程及 Go 子进程的 `oom_score_adj` 锁定为 `-1000`；
   - 自动解冻 Cgroup v2 `cgroup.freeze` 并将所有线程迁移至根 Cgroup（逃逸 ColorOS Hans 冻结）；
   - 意外退出时自动通过 `am start-foreground-service` 静默拉起。
3. 局域网/热点设备或异地组网设备可直接通过 `http://192.168.100.2:5244` 访问宿主机上的 Alist Lite。

### 方案 B：将手机目录直通挂载进 OpenWrt 内部运行原生 OpenList/Alist
1. 在手机文件管理器中创建 `/sdcard/NAS` 文件夹（对应底层物理 F2FS 路径 `/data/media/0/NAS`）；
2. 在 SuKiSU WebUI 的 **“路由与NAS配置”** 中开启 **“启用本机共享目录挂载”**，路径保持 `/data/media/0/NAS`；
3. OpenWrt 启动时会自动通过 `virtio-fs`（`crosvm` 模式）或 `virtio-9p`（`QEMU` 模式）将该目录挂载到 OpenWrt 内部的 **`/mnt/nas`**；
4. 进入 OpenWrt LuCI -> **服务 -> OpenList**，勾选启用，并在 OpenList 后台添加本地存储路径 `/mnt/nas` 即可！

---

## 四、 OpenVPN 异地组网（与家庭/机房双向互访）指南

1. **在手机 OpenWrt 中导入客户端配置**：
   - 登录 `http://192.168.100.1` -> **VPN -> OpenVPN**；
   - 上传家里或机房导出的 `.ovpn` 配置文件，启动连接；
   - `x-route` 固件已预置 `vpn` 防火墙区域（自动绑定 `tun+`），连接成功后，**手机本机与热点下的电脑即可直接访问家里/机房内网**。
2. **开启反向访问（让家里/机房内网直接访问手机热点下的设备）**：
   - 在家里/机房的 OpenVPN 服务端配置文件中添加：
     ```text
     client-config-dir /etc/openvpn/ccd
     route 192.168.100.0 255.255.255.0
     ```
   - 在服务端 `/etc/openvpn/ccd/<手机客户端证书CN名>` 文件中添加一行：
     ```text
     iroute 192.168.100.0 255.255.255.0
     ```
   - 重启服务端后，家里/机房任意电脑即可直接 ping 通并访问 `192.168.100.1`（OpenWrt）、`192.168.100.2`（一加 13 宿主机）以及 `192.168.100.X`（手机热点下的所有客户端）。

---

## 五、 Termux 辅助调试与高级环境配置手册（可选）

> **注意**：`x-route` 模块在默认 **Gunyah (`crosvm`)** 模式下直接使用系统内置 `/apex/com.android.virt/bin/crosvm`（或放入 `/data/adb/x-route/bin/crosvm` 的增强版二进制），**日常运行完全不依赖 Termux**。仅当您希望使用交互式命令行调试或使用 QEMU 备选后端时，可按以下步骤配置 Termux：

1. 安装 F-Droid 版 **Termux**，打开后执行：
   ```bash
   pkg update && pkg upgrade -y
   pkg install -y root-repo
   pkg install -y qemu-system-aarch64-headless qemu-utils iproute2 iw tcpdump curl
   ```
2. 若希望使用对高通 Gunyah 支持更完善的 [`Droid-VM/crosvm`](https://github.com/Droid-VM/crosvm) 二进制或预留物理大页模块 [`gh-hugepage-reserve`](https://github.com/Droid-VM/gh-hugepage-reserve)：
   - 可将静态编译的 `crosvm` 放入 `/data/adb/x-route/bin/crosvm` 并赋予执行权限（`chmod 755 /data/adb/x-route/bin/crosvm`），`vm_manager.sh` 会自动优先调用该二进制。
3. 手动在 Root Shell 中调试模块命令：
   ```bash
   su
   # 查看模块整体运行状态 (JSON)
   sh /data/adb/modules/x_route/action.sh status-json

   # 单独重启网络编排或同步热点网桥
   sh /data/adb/modules/x_route/scripts/net_orchestrator.sh reload

   # 查看 OpenWrt 虚拟机实时串口日志
   tail -f /data/adb/x-route/logs/vm-console.log
   ```
