# 第 6 步：固件（主机名、型号、包管理中文）

## 目标

ImmortalWrt 概览与 LuCI 展示：

- 主机名：`XRouter`
- 设备型号：`AMD Ryzen Threadripper PRO 9995WX`
- 包管理页面中文：把 `luci-i18n-package-manager-zh-cn` 打进固件，系统默认简体中文；上游未译完的条目再补翻译。

顺带确认第 4、5 步写入 `openwrt/files` 的单臂 NAT、网点防火墙、vpn 代理、控制目录挂载、init 更名都在镜像里。

不预装 OpenList，不预装 easy-rsa，不修改 root 密码。不在 OpenWrt 里做 Wi-Fi / 热点开关。

## 依赖

- 依赖第 1 步的 init / uci-defaults 更名，以及第 4、5 步对 `openwrt/files` 的修改。
- 本步完成后需要重新运行 `scripts/build-rootfs.sh`（或至少在第 8 步前完成），QEMU 才能测到客侧变更。内核若无变更不必重编。

## 要改的文件

- `openwrt/packages.txt`：增加 `luci-i18n-package-manager-zh-cn`（若 `luci-app-package-manager` 不在默认包中，一并显式列入）
- `openwrt/files/etc/uci-defaults/10-xrouter`：`hostname='XRouter'`，保持 `luci.main.lang='zh_cn'` 与 `imm_init.lang` 标记
- 新增开机脚本或 `board.d`：每次启动写入型号（`/tmp` 会丢失，不能只在 uci-defaults 写一次 `/tmp/sysinfo/model`）
- 如需补译：`openwrt/files/usr/share/rpcd/acl.d/` 或 LuCI 的 lmo/po/js 覆盖文件（以 25.12 实际路径为准）
- `scripts/build-rootfs.sh`：存在性检查加入新文件、语言包；禁止项仍包括 `easyrsa`、`opkg`、`htop`、`owut`
- `tests/e2e-qemu.sh`：断言主机名、型号、`luci.main.lang=zh_cn`，以及包管理 i18n 文件存在

## 具体做法

### 1. 主机名

uci-defaults 设置 `system.@system[0].hostname='XRouter'`。不要设成 `xrouter` 或 `x-route`。

### 2. 设备型号

`ubus call system board` / LuCI 概览读取 `/tmp/sysinfo/model`（由 `board_detect` / `board.d` 填充）。在 overlay 增加例如 `openwrt/files/etc/board.d/99-xrouter-model` 或独立 init（`START` 足够早），保证每次开机后：

```
/tmp/sysinfo/model = AMD Ryzen Threadripper PRO 9995WX
```

必要时同时设置 `board_name` 或 `DEVICE_PRODUCT`，以 LuCI 概览页实际展示为准。不要改成其它 CPU 名称。

### 3. 包管理中文

- `packages.txt` 加入 `luci-i18n-package-manager-zh-cn`。
- 默认语言已是 `zh_cn`，保持，避免被 `99-default-settings` 改回 `auto`（已有 `imm_init.lang` 标记，不要丢掉）。
- 构建或查阅上游 po 后，对包管理页仍为英文的可见字符串，在 `openwrt/files` 增加补译（简体中文，措辞简洁）。不要改英文本体逻辑文件，只补 i18n。
- 实现时先确认 ImmortalWrt 25.12.2 中该 i18n 包的确切包名；若仓库中名字不同，用实际可用的包名，并在第 8 步偏差里写一句。

### 4. 构建检查

`build-rootfs.sh` 在 rootfs 中断言：

- 存在：`/etc/uci-defaults/10-xrouter`、`/etc/init.d/xrouter-net`、`xrouter-ready`、`xrouter-share`、第 5 步的 vpn 代理与 up/down 脚本
- 存在语言包文件（以 apk 数据库或 `/usr/lib/lua/luci/i18n/`、LuCI 25 的实际路径为准）
- 不存在：`/usr/bin/easyrsa`、opkg、htop、owut
- inittab 仍无 `hvc0` 登录行（以免占用控制通道）

本步提交可以只改源文件与检查脚本，不必把编出来的 `dist/openwrt/*` 提交进库。第 8 步在本机或 CI 环境构建后做 QEMU。

## 验收标准

- 源文件中主机名为 `XRouter`，型号字符串完整准确。
- `packages.txt` 含包管理中文语言包，无 easy-rsa、无 OpenList。
- 若本机已具备 ImageBuilder 与已编内核，能成功 `bash scripts/build-rootfs.sh`；不能构建时不得假装已在 rootfs 里验证，留给第 8 步如实记录。
- `sh scripts/lint.sh` 通过。
